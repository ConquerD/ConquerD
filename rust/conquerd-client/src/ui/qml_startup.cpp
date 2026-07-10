// qml_startup.cpp — Qt/QML startup diagnostics for the native client.
//
// Mirrors the app_icon.cpp pattern: plain C++ compiled via `cc` in build.rs
// and called from main.rs through extern "C" shims.

#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQuickWindow>
#include <QWindow>
#include <cstdio>

#if defined(Q_OS_WIN)
// Defined in window_chrome.cpp (linked only on Windows qt-ui builds).
extern "C" void conquerd_enable_windows_snap(void *qwindow_ptr);
#endif

static void qtLogToStderr(QtMsgType type, const QMessageLogContext &ctx, const QString &msg) {
    const char *level = "INFO";
    switch (type) {
    case QtDebugMsg:    level = "DEBUG"; break;
    case QtInfoMsg:     level = "INFO";  break;
    case QtWarningMsg:  level = "WARN";  break;
    case QtCriticalMsg: level = "CRIT";  break;
    case QtFatalMsg:    level = "FATAL"; break;
    }
    if (ctx.category && ctx.file && ctx.line > 0) {
        fprintf(stderr, "[QT %s] %s (%s:%d)\n",
                level, msg.toUtf8().constData(), ctx.file, ctx.line);
    } else {
        fprintf(stderr, "[QT %s] %s\n", level, msg.toUtf8().constData());
    }
    if (type == QtFatalMsg) {
        abort();
    }
}

extern "C" void conquerd_install_qt_message_handler(void) {
    qInstallMessageHandler(qtLogToStderr);
}

extern "C" void conquerd_qml_post_load_check(QQmlApplicationEngine *engine) {
    if (!engine) {
        fprintf(stderr, "[QML] engine pointer is null\n");
        return;
    }

    engine->setOutputWarningsToStandardError(true);

    const auto roots = engine->rootObjects();
    fprintf(stderr, "[QML] root object count: %d\n", static_cast<int>(roots.size()));

    bool sawWindow = false;
    for (QObject *obj : roots) {
        if (!obj) {
            continue;
        }
        fprintf(stderr, "[QML] root object: %s\n", obj->metaObject()->className());

        if (auto *quickWin = qobject_cast<QQuickWindow *>(obj)) {
            sawWindow = true;
            fprintf(stderr,
                    "[QML] QQuickWindow visible=%d size=%dx%d pos=(%d,%d)\n",
                    quickWin->isVisible(),
                    quickWin->width(),
                    quickWin->height(),
                    quickWin->x(),
                    quickWin->y());
#if defined(Q_OS_WIN)
            // Force HWND creation then install snap-friendly frame chrome.
            (void)quickWin->winId();
            conquerd_enable_windows_snap(static_cast<QWindow *>(quickWin));
#endif
        } else if (auto *win = qobject_cast<QWindow *>(obj)) {
            sawWindow = true;
            fprintf(stderr,
                    "[QML] QWindow visible=%d size=%dx%d pos=(%d,%d)\n",
                    win->isVisible(),
                    win->width(),
                    win->height(),
                    win->x(),
                    win->y());
#if defined(Q_OS_WIN)
            (void)win->winId();
            conquerd_enable_windows_snap(win);
#endif
        }
    }

    if (roots.isEmpty()) {
        fprintf(stderr,
                "[QML] ERROR: MainWindow failed to load — no root objects were created\n");
    } else if (!sawWindow) {
        fprintf(stderr,
                "[QML] WARN: root objects exist but none are QWindow/QQuickWindow\n");
    }
}