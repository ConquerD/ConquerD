// Rust -> QML video frame delivery.
//
// QML's `VideoOutput` exposes a read-only `videoSink` property, so a sink
// cannot be assigned from QML or from Rust. The only workable direction is the
// one this registry implements: QML hands its sink to C++, keyed by peer id,
// and the decode thread pushes frames in.
//
// Everything crossing the cxx-qt boundary is a QString or a scalar, so the
// pixel path deliberately does not go through it — the plain `extern "C"`
// functions below take raw plane pointers instead.

#pragma once

#include <QtCore/QHash>
#include <QtCore/QObject>
#include <QtCore/QPointer>
#include <QtCore/QString>
#include <QtCore/QVector>
#include <QtMultimedia/QVideoFrame>
#include <QtMultimedia/QVideoSink>

#include <cstdint>

class ConquerdVideoRegistry : public QObject {
  Q_OBJECT

public:
  static ConquerdVideoRegistry *instance();

  /// Bind a QML `VideoOutput`'s sink to `peerId`.
  ///
  /// Several sinks may be registered for one peer at once — the expand region
  /// and a popout window can show the same stream — so this appends rather
  /// than replaces. `QVideoFrame` is implicitly shared, making the fan-out a
  /// refcount bump rather than a copy.
  Q_INVOKABLE void registerSink(const QString &peerId, QObject *videoOutput);

  /// Unbind a sink. Must be called from `Component.onDestruction`; the
  /// QPointer guard below is belt-and-braces, not the primary path.
  Q_INVOKABLE void unregisterSink(const QString &peerId, QObject *videoOutput);

  /// Whether any sink is currently bound for `peerId`. Lets the UI avoid
  /// requesting a keyframe for a stream nobody is watching.
  Q_INVOKABLE bool hasSink(const QString &peerId) const;

  /// Deliver a frame to every sink bound to `peerId`. GUI thread only.
  void fanOut(const QString &peerId, const QVideoFrame &frame);

  /// Blank every sink bound to `peerId` without unregistering them.
  ///
  /// Used when a peer turns their camera off or leaves: the last decoded
  /// frame must not stick on the tile, but the binding must stay so a later
  /// re-on (or a rejoin) can push frames without the QML side re-registering.
  /// GUI thread only.
  void clearPeer(const QString &peerId);

  /// Blank every registered sink. GUI thread only. Used on leave-room /
  /// end-call so no peer's last frame survives into the next session.
  void clearAll();

  /// Decrement the in-flight counter used by the backlog guard.
  void releaseInFlight(const QString &peerId);

  /// Try to reserve a slot in the delivery queue for `peerId`.
  ///
  /// Returns false when too many frames are already queued for the GUI thread,
  /// which is the caller's cue to drop this frame. Without this a stalled GUI
  /// thread (a modal dialog, a long-running binding) turns into unbounded
  /// queued-frame memory growth at roughly a megabyte a second per peer.
  bool tryReserveInFlight(const QString &peerId);

private:
  explicit ConquerdVideoRegistry(QObject *parent = nullptr) : QObject(parent) {}

  QHash<QString, QVector<QPointer<QVideoSink>>> m_sinks;
  QHash<QString, int> m_inFlight;
};

/// QML-callable frameless-chrome control for dynamically-created windows.
///
/// MainWindow receives chrome via `qml_startup.cpp`, which walks the engine's
/// root objects. A popout is created at runtime and is therefore never a root
/// object, so it has to ask for chrome itself — and, just as importantly,
/// give it back on destruction.
///
/// This lives beside the video registry only because that shim already runs
/// moc; the two are otherwise unrelated.
class ConquerdWindowChrome : public QObject {
  Q_OBJECT

public:
  static ConquerdWindowChrome *instance();

  /// Apply frameless snap chrome to `window`. No-op off Windows.
  Q_INVOKABLE void enable(QObject *window);

  /// Stop tracking `window`. Must be called on destruction — Windows recycles
  /// HWND values, so a stale entry would later apply our frame handling to an
  /// unrelated window.
  Q_INVOKABLE void disable(QObject *window);

private:
  explicit ConquerdWindowChrome(QObject *parent = nullptr) : QObject(parent) {}
};

extern "C" {

/// Register the registry and chrome helper as QML singletons.
/// Call before `engine.load()`.
void conquerd_register_video_singleton();

/// Push one tightly-packed I420 frame for `peer_id`.
///
/// Safe to call from any thread: the frame is built here (allocation and copy
/// belong off the GUI thread) and then hopped to the GUI thread for fan-out.
void conquerd_video_push_i420(const char *peer_id, int width, int height,
                              const uint8_t *y, const uint8_t *u,
                              const uint8_t *v);

/// Blank every sink for a peer (keep registrations), e.g. camera off / leave.
void conquerd_video_clear(const char *peer_id);

/// Blank every registered sink. Used when the local session ends.
void conquerd_video_clear_all();

/// Whether anything is currently displaying this peer. Lets the decode thread
/// skip work for peers whose tiles are closed.
bool conquerd_video_has_sink(const char *peer_id);

} // extern "C"
