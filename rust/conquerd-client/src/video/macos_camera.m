/*
 * AVFoundation camera capture, behind a small synchronous C API.
 *
 * Why Objective-C rather than objc2 bindings in Rust: AVFoundation delivers
 * frames by *calling you*. `AVCaptureVideoDataOutput` has no pull interface —
 * it invokes `captureOutput:didOutputSampleBuffer:fromConnection:` on a
 * dispatch queue, so a Rust implementation has to declare an Objective-C class
 * conforming to a protocol and hand its selector table to the runtime. That is
 * possible but it is a lot of unsafe binding machinery to express something
 * the language it is written in says in twenty lines.
 *
 * This file therefore owns the delegate and the push-to-pull adaptation, and
 * exposes what the rest of the pipeline actually wants: "block until the next
 * frame, give me I420". Same reasoning as `conquerd-vpx/src/shim.c` — the FFI
 * boundary sits where it makes the Rust side trivial.
 *
 * All state lives on one Objective-C object rather than in a malloc'd C struct.
 * That is not a style preference: under ARC, Objective-C pointers inside a C
 * struct make it a "non-trivial" struct whose copy/destroy helpers `calloc` and
 * `free` do not run, so every session would leak its capture graph. Handing out
 * an opaque bridged pointer keeps ARC in charge of the object graph and leaves
 * the C boundary holding nothing it has to manage.
 *
 * Threading: the delegate runs on its own serial queue and hands the newest
 * frame to a mutex-protected slot. Deliberately a slot and not a queue: video
 * is real-time, so if the consumer is behind, the right frame to keep is the
 * newest one, and a backlog would only add latency.
 */

#import <AVFoundation/AVFoundation.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>

#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <time.h>

/* Kept in sync with the Rust side; see the macos_impl module in camera.rs. */
#define CQ_CAM_ERR -1
#define CQ_CAM_TIMEOUT -2
#define CQ_CAM_TOO_SMALL -3

@interface CqCamera : NSObject <AVCaptureVideoDataOutputSampleBufferDelegate> {
  pthread_mutex_t _lock;
  pthread_cond_t _cond;
  uint8_t *_frame;
  size_t _frame_cap;
  size_t _frame_len;
  int _width;
  int _height;
  int _have_frame;
  int _stopped;
}
@property(nonatomic, strong) AVCaptureSession *session;
@property(nonatomic, strong) AVCaptureVideoDataOutput *output;
@property(nonatomic, strong) dispatch_queue_t queue;

- (int)copyFrameInto:(uint8_t *)out
                 cap:(int)cap
               width:(int *)width
              height:(int *)height
           timeoutMs:(int)timeout_ms;
- (void)stop;
@end

@implementation CqCamera

- (instancetype)init {
  self = [super init];
  if (self) {
    pthread_mutex_init(&_lock, NULL);
    pthread_cond_init(&_cond, NULL);
    _frame = NULL;
    _frame_cap = 0;
    _frame_len = 0;
    _width = 0;
    _height = 0;
    _have_frame = 0;
    _stopped = 0;
  }
  return self;
}

- (void)dealloc {
  if (_frame) free(_frame);
  pthread_cond_destroy(&_cond);
  pthread_mutex_destroy(&_lock);
}

- (void)stop {
  /* Wake any blocked reader before tearing down, or next_frame would wait out
   * its whole timeout against a session that is already stopping. */
  pthread_mutex_lock(&_lock);
  _stopped = 1;
  pthread_cond_broadcast(&_cond);
  pthread_mutex_unlock(&_lock);

  [_session stopRunning];
  [_output setSampleBufferDelegate:nil queue:NULL];
  _session = nil;
  _output = nil;
  _queue = nil;
}

- (void)captureOutput:(AVCaptureOutput *)output
    didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
           fromConnection:(AVCaptureConnection *)connection {
  (void)output;
  (void)connection;

  CVImageBufferRef pixels = CMSampleBufferGetImageBuffer(sampleBuffer);
  if (!pixels) return;
  if (CVPixelBufferLockBaseAddress(pixels, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) {
    return;
  }

  const int w = (int)CVPixelBufferGetWidth(pixels);
  const int h = (int)CVPixelBufferGetHeight(pixels);
  const OSType fmt = CVPixelBufferGetPixelFormatType(pixels);
  /* Odd sizes have no valid 4:2:0 chroma plane. */
  if (w <= 0 || h <= 0 || (w % 2) || (h % 2)) {
    CVPixelBufferUnlockBaseAddress(pixels, kCVPixelBufferLock_ReadOnly);
    return;
  }

  const int cw = w / 2, ch = h / 2;
  const size_t needed = (size_t)w * h + 2 * (size_t)cw * ch;

  pthread_mutex_lock(&_lock);
  if (_frame_cap < needed) {
    uint8_t *grown = (uint8_t *)realloc(_frame, needed);
    if (!grown) {
      pthread_mutex_unlock(&_lock);
      CVPixelBufferUnlockBaseAddress(pixels, kCVPixelBufferLock_ReadOnly);
      return;
    }
    _frame = grown;
    _frame_cap = needed;
  }

  int ok = 1;
  if (fmt == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange ||
      fmt == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange) {
    /* NV12: plane 0 luma, plane 1 interleaved UV. */
    const uint8_t *y_src = (const uint8_t *)CVPixelBufferGetBaseAddressOfPlane(pixels, 0);
    const size_t y_stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 0);
    const uint8_t *uv_src = (const uint8_t *)CVPixelBufferGetBaseAddressOfPlane(pixels, 1);
    const size_t uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 1);
    if (!y_src || !uv_src) {
      ok = 0;
    } else {
      for (int r = 0; r < h; ++r) {
        memcpy(_frame + (size_t)r * w, y_src + (size_t)r * y_stride, (size_t)w);
      }
      uint8_t *u_dst = _frame + (size_t)w * h;
      uint8_t *v_dst = u_dst + (size_t)cw * ch;
      for (int r = 0; r < ch; ++r) {
        const uint8_t *row = uv_src + (size_t)r * uv_stride;
        for (int c = 0; c < cw; ++c) {
          u_dst[(size_t)r * cw + c] = row[c * 2];
          v_dst[(size_t)r * cw + c] = row[c * 2 + 1];
        }
      }
    }
  } else if (fmt == kCVPixelFormatType_420YpCbCr8Planar ||
             fmt == kCVPixelFormatType_420YpCbCr8PlanarFullRange) {
    /* Already I420, but the rows may be padded. */
    const uint8_t *sy = (const uint8_t *)CVPixelBufferGetBaseAddressOfPlane(pixels, 0);
    const uint8_t *su = (const uint8_t *)CVPixelBufferGetBaseAddressOfPlane(pixels, 1);
    const uint8_t *sv = (const uint8_t *)CVPixelBufferGetBaseAddressOfPlane(pixels, 2);
    const size_t ys = CVPixelBufferGetBytesPerRowOfPlane(pixels, 0);
    const size_t us = CVPixelBufferGetBytesPerRowOfPlane(pixels, 1);
    const size_t vs = CVPixelBufferGetBytesPerRowOfPlane(pixels, 2);
    if (!sy || !su || !sv) {
      ok = 0;
    } else {
      for (int r = 0; r < h; ++r)
        memcpy(_frame + (size_t)r * w, sy + (size_t)r * ys, (size_t)w);
      uint8_t *u_dst = _frame + (size_t)w * h;
      uint8_t *v_dst = u_dst + (size_t)cw * ch;
      for (int r = 0; r < ch; ++r) {
        memcpy(u_dst + (size_t)r * cw, su + (size_t)r * us, (size_t)cw);
        memcpy(v_dst + (size_t)r * cw, sv + (size_t)r * vs, (size_t)cw);
      }
    }
  } else {
    /* The output was configured for one of the above; anything else means
     * AVFoundation substituted a format, and guessing at its layout would
     * produce a corrupt picture rather than an error. */
    ok = 0;
  }

  if (ok) {
    _frame_len = needed;
    _width = w;
    _height = h;
    _have_frame = 1;
    pthread_cond_signal(&_cond);
  }
  pthread_mutex_unlock(&_lock);
  CVPixelBufferUnlockBaseAddress(pixels, kCVPixelBufferLock_ReadOnly);
}

- (int)copyFrameInto:(uint8_t *)out
                 cap:(int)cap
               width:(int *)width
              height:(int *)height
           timeoutMs:(int)timeout_ms {
  struct timespec deadline;
  clock_gettime(CLOCK_REALTIME, &deadline);
  deadline.tv_sec += timeout_ms / 1000;
  deadline.tv_nsec += (long)(timeout_ms % 1000) * 1000000L;
  if (deadline.tv_nsec >= 1000000000L) {
    deadline.tv_sec += 1;
    deadline.tv_nsec -= 1000000000L;
  }

  pthread_mutex_lock(&_lock);
  while (!_have_frame && !_stopped) {
    if (pthread_cond_timedwait(&_cond, &_lock, &deadline) != 0) {
      pthread_mutex_unlock(&_lock);
      return CQ_CAM_TIMEOUT;
    }
  }
  if (_stopped) {
    pthread_mutex_unlock(&_lock);
    return CQ_CAM_ERR;
  }

  const int len = (int)_frame_len;
  if (len > cap) {
    /* Report the size so the caller can resize exactly rather than guess. */
    *width = _width;
    *height = _height;
    pthread_mutex_unlock(&_lock);
    return CQ_CAM_TOO_SMALL;
  }
  memcpy(out, _frame, (size_t)len);
  *width = _width;
  *height = _height;
  /* Consumed: the next call waits for a genuinely new frame rather than
   * handing back the same one and pretending the camera is faster than it is. */
  _have_frame = 0;
  pthread_mutex_unlock(&_lock);
  return len;
}

@end

/* ---- Device enumeration ------------------------------------------------ */

static NSArray<AVCaptureDevice *> *cq_video_devices(void) {
  /* External cameras are a separate device type and are the common case on a
   * desktop Mac, so discovery has to name both. */
  NSMutableArray<AVCaptureDeviceType> *types = [NSMutableArray array];
  [types addObject:AVCaptureDeviceTypeBuiltInWideAngleCamera];
  if (@available(macOS 14.0, *)) {
    [types addObject:AVCaptureDeviceTypeExternal];
  } else {
    /* Deprecated in 14 but the only way to see USB cameras before it. */
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    [types addObject:AVCaptureDeviceTypeExternalUnknown];
#pragma clang diagnostic pop
  }
  AVCaptureDeviceDiscoverySession *session = [AVCaptureDeviceDiscoverySession
      discoverySessionWithDeviceTypes:types
                            mediaType:AVMediaTypeVideo
                             position:AVCaptureDevicePositionUnspecified];
  return session.devices;
}

/*
 * Write device ids and names as NUL-separated pairs into `buf`.
 *
 * A flat buffer rather than an array of structs so the Rust side does no
 * pointer arithmetic over Objective-C memory and nothing needs freeing across
 * the boundary. Returns the number of devices, or the negative byte count
 * needed when `cap` is too small.
 */
int cq_mac_cam_list(char *buf, int cap) {
  @autoreleasepool {
    NSArray<AVCaptureDevice *> *devices = cq_video_devices();
    int used = 0;
    int count = 0;
    for (AVCaptureDevice *d in devices) {
      const char *uid = [[d uniqueID] UTF8String];
      const char *name = [[d localizedName] UTF8String];
      if (!uid || !name) continue;
      const int need = (int)strlen(uid) + 1 + (int)strlen(name) + 1;
      if (used + need > cap) return -(used + need);
      memcpy(buf + used, uid, strlen(uid) + 1);
      used += (int)strlen(uid) + 1;
      memcpy(buf + used, name, strlen(name) + 1);
      used += (int)strlen(name) + 1;
      count++;
    }
    return count;
  }
}

/* ---- Capture lifecycle -------------------------------------------------- */

/* Returns an opaque, retained handle. Ownership transfers to the caller, who
 * must return it through cq_mac_cam_free exactly once. */
void *cq_mac_cam_open(const char *device_id, int width, int height) {
  @autoreleasepool {
    AVCaptureDevice *device = nil;
    if (device_id && device_id[0]) {
      device = [AVCaptureDevice deviceWithUniqueID:[NSString stringWithUTF8String:device_id]];
    }
    if (!device) {
      NSArray<AVCaptureDevice *> *all = cq_video_devices();
      if (all.count == 0) return NULL;
      device = all[0];
    }

    NSError *err = nil;
    AVCaptureDeviceInput *input = [AVCaptureDeviceInput deviceInputWithDevice:device error:&err];
    if (!input) return NULL;

    CqCamera *cam = [[CqCamera alloc] init];
    cam.session = [[AVCaptureSession alloc] init];
    if (![cam.session canAddInput:input]) return NULL;
    [cam.session addInput:input];

    cam.output = [[AVCaptureVideoDataOutput alloc] init];
    /* NV12 is what Apple hardware produces natively; asking for it avoids a
     * hidden conversion inside AVFoundation. */
    cam.output.videoSettings = @{
      (NSString *)kCVPixelBufferPixelFormatTypeKey :
          @(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
    };
    /* Drop late frames rather than queueing them: see the threading note. */
    cam.output.alwaysDiscardsLateVideoFrames = YES;

    cam.queue = dispatch_queue_create("com.conquerd.camera", DISPATCH_QUEUE_SERIAL);
    [cam.output setSampleBufferDelegate:cam queue:cam.queue];

    if (![cam.session canAddOutput:cam.output]) return NULL;
    [cam.session addOutput:cam.output];

    /* Ask for a preset near the requested size. AVFoundation presets are
     * coarse, and the delegate reports whatever actually arrives, so the
     * caller must read back the real dimensions rather than assume. */
    if (width <= 640 && height <= 480 &&
        [cam.session canSetSessionPreset:AVCaptureSessionPreset640x480]) {
      cam.session.sessionPreset = AVCaptureSessionPreset640x480;
    } else if (width <= 1280 && height <= 720 &&
               [cam.session canSetSessionPreset:AVCaptureSessionPreset1280x720]) {
      cam.session.sessionPreset = AVCaptureSessionPreset1280x720;
    } else if ([cam.session canSetSessionPreset:AVCaptureSessionPresetHigh]) {
      cam.session.sessionPreset = AVCaptureSessionPresetHigh;
    }

    [cam.session startRunning];
    return (__bridge_retained void *)cam;
  }
}

void cq_mac_cam_free(void *handle) {
  if (!handle) return;
  @autoreleasepool {
    /* __bridge_transfer takes ownership back, so ARC releases `cam` at the end
     * of this scope — after the session has been stopped. */
    CqCamera *cam = (__bridge_transfer CqCamera *)handle;
    [cam stop];
  }
}

/*
 * Block until a frame is available, then copy it out as tightly-packed I420.
 *
 * Returns bytes written, or one of the CQ_CAM_* codes. `timeout_ms` bounds the
 * wait so a camera that stops delivering (unplugged, or permission revoked
 * mid-session) surfaces as an error instead of hanging the capture thread.
 */
int cq_mac_cam_next_frame(void *handle, uint8_t *out, int cap, int *width, int *height,
                          int timeout_ms) {
  if (!handle || !out || !width || !height) return CQ_CAM_ERR;
  CqCamera *cam = (__bridge CqCamera *)handle;
  return [cam copyFrameInto:out cap:cap width:width height:height timeoutMs:timeout_ms];
}
