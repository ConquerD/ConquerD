// window_chrome.cpp — Windows 10/11 Aero Snap for the custom title bar.
//
// Problem: `Qt.CustomizeWindowHint` alone omits WS_CAPTION. Without a caption
// style the DWM snap engine ignores drag-to-edge. This helper:
//
//   1. Restores WS_CAPTION | WS_THICKFRAME | min/max/sysmenu (snap-friendly).
//   2. Handles WM_NCCALCSIZE so the native title bar does not consume layout
//      space — TitleBar.qml owns chrome in the client area.
//   3. Handles WM_NCHITTEST only for resize borders (edges/corners). The QML
//      title bar keeps HTCLIENT so interactive widgets work; it calls
//      startSystemMove() which now sees a snap-capable window style.
//
// Called once after MainWindow.qml loads (see main.rs).

#include <QAbstractNativeEventFilter>
#include <QGuiApplication>
#include <QByteArray>
#include <QWindow>

#include <cstdio>

#if defined(Q_OS_WIN)
#  ifndef NOMINMAX
#    define NOMINMAX
#  endif
#  include <windows.h>
#  include <windowsx.h>
#  include <dwmapi.h>
#  pragma comment(lib, "dwmapi.lib")
#endif

namespace {

// Resize border thickness in logical (device-independent) pixels.
constexpr int kResizeBorderDip = 6;

#if defined(Q_OS_WIN)

static QWindow *g_mainWindow = nullptr;
static HWND g_mainHwnd = nullptr;

static int dipToPx(const QWindow *win, int dip)
{
    if (!win) {
        return dip;
    }
    return qMax(1, qRound(static_cast<qreal>(dip) * win->devicePixelRatio()));
}

static bool isOurWindow(HWND hwnd)
{
    return g_mainHwnd && hwnd == g_mainHwnd;
}

static void applySnapFriendlyStyle(HWND hwnd)
{
    if (!hwnd) {
        return;
    }

    // Overlapped + thick frame + caption is what the DWM snap engine expects.
    // Caption height is removed via WM_NCCALCSIZE so only our QML bar shows.
    LONG_PTR style = GetWindowLongPtrW(hwnd, GWL_STYLE);
    style &= ~(LONG_PTR)WS_POPUP;
    style |= (LONG_PTR)(WS_OVERLAPPED | WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX
                        | WS_MAXIMIZEBOX | WS_SYSMENU | WS_CLIPCHILDREN
                        | WS_CLIPSIBLINGS);
    SetWindowLongPtrW(hwnd, GWL_STYLE, style);

    // Keep a DWM shadow without reintroducing a classic non-client caption.
    MARGINS margins = {1, 1, 1, 1};
    DwmExtendFrameIntoClientArea(hwnd, &margins);

    SetWindowPos(hwnd, nullptr, 0, 0, 0, 0,
                 SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER
                     | SWP_NOOWNERZORDER | SWP_NOACTIVATE);
}

class SnapNativeFilter : public QAbstractNativeEventFilter
{
public:
    bool nativeEventFilter(const QByteArray &eventType, void *message,
                           qintptr *result) override
    {
        if (eventType != "windows_generic_MSG"
            && eventType != "windows_dispatcher_MSG") {
            return false;
        }
        auto *msg = static_cast<MSG *>(message);
        if (!msg || !isOurWindow(msg->hwnd)) {
            return false;
        }

        switch (msg->message) {
        case WM_NCCALCSIZE:
            return handleNcCalcSize(msg, result);
        case WM_NCHITTEST:
            return handleNcHitTest(msg, result);
        case WM_NCACTIVATE:
            // No native caption to paint; claim handled to avoid flicker.
            if (result) {
                *result = TRUE;
            }
            return true;
        case WM_NCPAINT:
            if (result) {
                *result = 0;
            }
            return true;
        default:
            return false;
        }
    }

private:
    static bool handleNcCalcSize(MSG *msg, qintptr *result)
    {
        if (msg->wParam != TRUE) {
            return false;
        }
        auto *params = reinterpret_cast<NCCALCSIZE_PARAMS *>(msg->lParam);
        if (!params) {
            return false;
        }

        // Full-window client area. When maximized, clamp to the monitor work
        // area so the window does not cover the taskbar.
        if (IsZoomed(msg->hwnd)) {
            HMONITOR mon = MonitorFromWindow(msg->hwnd, MONITOR_DEFAULTTONEAREST);
            MONITORINFO mi{};
            mi.cbSize = sizeof(mi);
            if (GetMonitorInfoW(mon, &mi)) {
                params->rgrc[0] = mi.rcWork;
            }
        }
        // else: leave rgrc[0] as the proposed window rect (= full client).

        if (result) {
            *result = 0;
        }
        return true;
    }

    static bool handleNcHitTest(MSG *msg, qintptr *result)
    {
        if (!g_mainWindow || !result) {
            return false;
        }

        // Only specialize resize grips. Everything else is HTCLIENT so QML
        // (including TitleBar startSystemMove) keeps receiving input.
        if (IsZoomed(msg->hwnd)) {
            *result = HTCLIENT;
            return true;
        }

        const int x = GET_X_LPARAM(msg->lParam);
        const int y = GET_Y_LPARAM(msg->lParam);

        RECT wr{};
        if (!GetWindowRect(msg->hwnd, &wr)) {
            return false;
        }

        const int border = dipToPx(g_mainWindow, kResizeBorderDip);
        const bool onLeft = x >= wr.left && x < wr.left + border;
        const bool onRight = x < wr.right && x >= wr.right - border;
        const bool onTop = y >= wr.top && y < wr.top + border;
        const bool onBottom = y < wr.bottom && y >= wr.bottom - border;

        if (onTop && onLeft) {
            *result = HTTOPLEFT;
            return true;
        }
        if (onTop && onRight) {
            *result = HTTOPRIGHT;
            return true;
        }
        if (onBottom && onLeft) {
            *result = HTBOTTOMLEFT;
            return true;
        }
        if (onBottom && onRight) {
            *result = HTBOTTOMRIGHT;
            return true;
        }
        if (onLeft) {
            *result = HTLEFT;
            return true;
        }
        if (onRight) {
            *result = HTRIGHT;
            return true;
        }
        if (onTop) {
            *result = HTTOP;
            return true;
        }
        if (onBottom) {
            *result = HTBOTTOM;
            return true;
        }

        *result = HTCLIENT;
        return true;
    }
};

static SnapNativeFilter g_filter;
static bool g_filterInstalled = false;

#endif // Q_OS_WIN

} // namespace

extern "C" void conquerd_enable_windows_snap(void *qwindow_ptr)
{
#if defined(Q_OS_WIN)
    auto *window = static_cast<QWindow *>(qwindow_ptr);
    if (!window) {
        fprintf(stderr, "[chrome] enable_windows_snap: null window\n");
        return;
    }

    const WId wid = window->winId();
    auto hwnd = reinterpret_cast<HWND>(wid);
    if (!hwnd) {
        fprintf(stderr, "[chrome] enable_windows_snap: winId() is null\n");
        return;
    }

    g_mainWindow = window;
    g_mainHwnd = hwnd;
    applySnapFriendlyStyle(hwnd);

    if (!g_filterInstalled) {
        if (auto *app = qApp) {
            app->installNativeEventFilter(&g_filter);
            g_filterInstalled = true;
        }
    }

    fprintf(stderr, "[chrome] Windows snap chrome enabled (hwnd=%p)\n",
            static_cast<void *>(hwnd));
#else
    (void)qwindow_ptr;
#endif
}
