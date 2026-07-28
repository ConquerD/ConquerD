#include "video_sink_bridge.h"

#include <QtCore/QDebug>
#include <QtCore/QMetaObject>
#include <QtGui/QWindow>
#include <QtQml/qqml.h>

#include <cstring>

namespace {

/// Maximum frames queued toward the GUI thread per peer before dropping.
///
/// Two is deliberate: enough to keep the pipeline busy across one slow paint,
/// small enough that a genuinely stalled GUI thread sheds frames immediately
/// instead of accumulating them.
constexpr int kMaxInFlightPerPeer = 2;

} // namespace

ConquerdVideoRegistry *ConquerdVideoRegistry::instance() {
  static ConquerdVideoRegistry registry;
  return &registry;
}

void ConquerdVideoRegistry::registerSink(const QString &peerId,
                                         QObject *videoOutput) {
  if (peerId.isEmpty() || videoOutput == nullptr) {
    return;
  }
  // QML hands us the VideoOutput item; the sink lives on its `videoSink`
  // property. Reading it via the property system avoids a compile-time
  // dependency on QtMultimediaQuick, which ships no public headers.
  QVariant v = videoOutput->property("videoSink");
  auto *sink = v.value<QVideoSink *>();
  if (sink == nullptr) {
    qWarning("[video] registerSink: object has no videoSink property");
    return;
  }

  auto &list = m_sinks[peerId];
  for (const auto &existing : list) {
    if (existing.data() == sink) {
      return; // already bound
    }
  }
  list.append(QPointer<QVideoSink>(sink));
}

void ConquerdVideoRegistry::unregisterSink(const QString &peerId,
                                           QObject *videoOutput) {
  if (peerId.isEmpty() || videoOutput == nullptr) {
    return;
  }
  QVariant v = videoOutput->property("videoSink");
  auto *sink = v.value<QVideoSink *>();

  auto it = m_sinks.find(peerId);
  if (it == m_sinks.end()) {
    return;
  }
  auto &list = it.value();
  for (int i = list.size() - 1; i >= 0; --i) {
    // Also reap sinks whose owning item has already been destroyed.
    if (list[i].isNull() || list[i].data() == sink) {
      list.removeAt(i);
    }
  }
  if (list.isEmpty()) {
    m_sinks.erase(it);
    m_inFlight.remove(peerId);
  }
}

bool ConquerdVideoRegistry::hasSink(const QString &peerId) const {
  auto it = m_sinks.constFind(peerId);
  if (it == m_sinks.constEnd()) {
    return false;
  }
  for (const auto &s : it.value()) {
    if (!s.isNull()) {
      return true;
    }
  }
  return false;
}

bool ConquerdVideoRegistry::tryReserveInFlight(const QString &peerId) {
  int &n = m_inFlight[peerId];
  if (n >= kMaxInFlightPerPeer) {
    return false;
  }
  ++n;
  return true;
}

void ConquerdVideoRegistry::releaseInFlight(const QString &peerId) {
  auto it = m_inFlight.find(peerId);
  if (it != m_inFlight.end() && it.value() > 0) {
    --it.value();
  }
}

void ConquerdVideoRegistry::fanOut(const QString &peerId,
                                   const QVideoFrame &frame) {
  auto it = m_sinks.find(peerId);
  if (it == m_sinks.end()) {
    return;
  }
  auto &list = it.value();
  for (int i = list.size() - 1; i >= 0; --i) {
    if (list[i].isNull()) {
      list.removeAt(i); // owning item destroyed without unregistering
      continue;
    }
    list[i]->setVideoFrame(frame);
  }
  if (list.isEmpty()) {
    m_sinks.erase(it);
  }
}

void ConquerdVideoRegistry::clearPeer(const QString &peerId) {
  // Blank the last frame but keep the registration: a peer who turns their
  // camera back on (or rejoins) must be able to push frames without the QML
  // tile re-running registerSink. Dead QPointers are pruned so a destroyed
  // tile still self-reaps.
  auto it = m_sinks.find(peerId);
  if (it != m_sinks.end()) {
    auto &list = it.value();
    for (int i = list.size() - 1; i >= 0; --i) {
      if (list[i].isNull()) {
        list.removeAt(i);
        continue;
      }
      list[i]->setVideoFrame(QVideoFrame());
    }
    if (list.isEmpty()) {
      m_sinks.erase(it);
    }
  }
  m_inFlight.remove(peerId);
}

void ConquerdVideoRegistry::clearAll() {
  for (auto it = m_sinks.begin(); it != m_sinks.end();) {
    auto &list = it.value();
    for (int i = list.size() - 1; i >= 0; --i) {
      if (list[i].isNull()) {
        list.removeAt(i);
        continue;
      }
      list[i]->setVideoFrame(QVideoFrame());
    }
    if (list.isEmpty()) {
      it = m_sinks.erase(it);
    } else {
      ++it;
    }
  }
  m_inFlight.clear();
}

// Implemented in window_chrome.cpp; declared here rather than in a shared
// header because these two shims are otherwise independent.
extern "C" void conquerd_enable_windows_snap(void *qwindow_ptr);
extern "C" void conquerd_disable_windows_snap(void *qwindow_ptr);

ConquerdWindowChrome *ConquerdWindowChrome::instance() {
  static ConquerdWindowChrome chrome;
  return &chrome;
}

void ConquerdWindowChrome::enable(QObject *window) {
#if defined(Q_OS_WIN)
  if (auto *w = qobject_cast<QWindow *>(window)) {
    conquerd_enable_windows_snap(w);
  }
#else
  Q_UNUSED(window);
#endif
}

void ConquerdWindowChrome::disable(QObject *window) {
#if defined(Q_OS_WIN)
  if (auto *w = qobject_cast<QWindow *>(window)) {
    conquerd_disable_windows_snap(w);
  }
#else
  Q_UNUSED(window);
#endif
}

extern "C" void conquerd_register_video_singleton() {
  qmlRegisterSingletonInstance("ConquerD.Native", 1, 0, "VideoRegistry",
                               ConquerdVideoRegistry::instance());
  qmlRegisterSingletonInstance("ConquerD.Native", 1, 0, "WindowChrome",
                               ConquerdWindowChrome::instance());
}

extern "C" void conquerd_video_push_i420(const char *peer_id, int width,
                                         int height, const uint8_t *y,
                                         const uint8_t *u, const uint8_t *v) {
  if (peer_id == nullptr || y == nullptr || u == nullptr || v == nullptr) {
    return;
  }
  if (width <= 0 || height <= 0 || (width & 1) || (height & 1)) {
    return;
  }

  const QString id = QString::fromUtf8(peer_id);
  auto *reg = ConquerdVideoRegistry::instance();

  // Shed before doing any work when the GUI thread is behind.
  if (!reg->tryReserveInFlight(id)) {
    return;
  }

  QVideoFrameFormat fmt(QSize(width, height),
                        QVideoFrameFormat::Format_YUV420P);
  QVideoFrame frame(fmt);
  if (!frame.map(QVideoFrame::WriteOnly)) {
    reg->releaseInFlight(id);
    return;
  }

  const int cw = width / 2;
  const int ch = height / 2;

  // Row-by-row, honouring each plane's own stride. The source is tightly
  // packed but QVideoFrame pads its rows, so a single memcpy per plane would
  // produce the classic diagonally-sheared picture — which looks "almost
  // right" and is therefore easy to ship.
  uint8_t *dstY = frame.bits(0);
  const int strideY = frame.bytesPerLine(0);
  for (int r = 0; r < height; ++r) {
    std::memcpy(dstY + static_cast<size_t>(r) * strideY,
                y + static_cast<size_t>(r) * width, static_cast<size_t>(width));
  }

  uint8_t *dstU = frame.bits(1);
  const int strideU = frame.bytesPerLine(1);
  for (int r = 0; r < ch; ++r) {
    std::memcpy(dstU + static_cast<size_t>(r) * strideU,
                u + static_cast<size_t>(r) * cw, static_cast<size_t>(cw));
  }

  uint8_t *dstV = frame.bits(2);
  const int strideV = frame.bytesPerLine(2);
  for (int r = 0; r < ch; ++r) {
    std::memcpy(dstV + static_cast<size_t>(r) * strideV,
                v + static_cast<size_t>(r) * cw, static_cast<size_t>(cw));
  }

  frame.unmap();

  // Hop to the GUI thread. A capturing lambda avoids needing
  // qRegisterMetaType<QVideoFrame>() for a queued signal argument.
  QMetaObject::invokeMethod(
      reg,
      [id, frame]() {
        auto *r = ConquerdVideoRegistry::instance();
        r->fanOut(id, frame);
        r->releaseInFlight(id);
      },
      Qt::QueuedConnection);
}

extern "C" bool conquerd_video_has_sink(const char *peer_id) {
  if (peer_id == nullptr) {
    return false;
  }
  // Read-only and racy by nature: a tile could open or close between this
  // check and the next frame. Worst case is one wasted or one skipped decode,
  // which self-corrects on the following frame.
  return ConquerdVideoRegistry::instance()->hasSink(QString::fromUtf8(peer_id));
}

extern "C" void conquerd_video_clear(const char *peer_id) {
  if (peer_id == nullptr) {
    return;
  }
  const QString id = QString::fromUtf8(peer_id);
  QMetaObject::invokeMethod(
      ConquerdVideoRegistry::instance(),
      [id]() {
        // Clearing on the GUI thread keeps all m_sinks access single-threaded.
        ConquerdVideoRegistry::instance()->clearPeer(id);
      },
      Qt::QueuedConnection);
}

extern "C" void conquerd_video_clear_all() {
  QMetaObject::invokeMethod(
      ConquerdVideoRegistry::instance(),
      []() { ConquerdVideoRegistry::instance()->clearAll(); },
      Qt::QueuedConnection);
}
