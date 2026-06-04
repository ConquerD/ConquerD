// scheme.cpp — conquerd:// custom URL scheme handler for QtWebEngine.
//
// Two entry points are exported with C linkage so Rust can call them:
//
//   conquerd_register_scheme()
//     Must be called BEFORE QGuiApplication::new().
//     Registers "conquerd" as a secure scheme so QtWebEngine treats it like
//     https:// (allows CORS, service workers, secure context APIs).
//
//   conquerd_install_scheme_handler()
//     Must be called AFTER QGuiApplication is created but BEFORE any
//     WebEngineView loads a conquerd:// URL.
//     Installs a ConquerdSchemeHandler on the default off-the-record
//     QWebEngineProfile so every WebEngineView in the process shares it.
//
// The handler calls the Rust function:
//
//   extern "C" bool conquerd_fetch_sync(
//       const char* url, size_t url_len,
//       char** out_content_type, size_t* out_ct_len,
//       uint8_t** out_body,     size_t* out_body_len
//   );
//
// Rust allocates the output buffers with malloc(); the handler frees them
// after writing the response. On failure (returns false) the handler serves
// a minimal HTML error page.

#include <QtWebEngineCore/QWebEngineProfile>
#include <QtWebEngineCore/QWebEngineUrlScheme>
#include <QtWebEngineCore/QWebEngineUrlSchemeHandler>
#include <QtWebEngineCore/QWebEngineUrlRequestJob>
#include <QtWebEngineCore/QWebEngineScript>
#include <QtWebEngineCore/QWebEngineScriptCollection>
#if __has_include(<QtWebEngineQuick/QQuickWebEngineProfile>)
#  include <QtWebEngineQuick/QQuickWebEngineProfile>
#  define CONQUERD_HAVE_QUICK_PROFILE 1
#endif
#include <QtCore/QByteArray>
#include <QtCore/QString>
#include <QtCore/QUrl>
#include <QtCore/QBuffer>
#include <QtCore/QLoggingCategory>
#include <cstdlib>
#include <cstdint>
#include <cstring>

// ── Rust callback declared in src/ui/scheme.rs ───────────────────────────────
extern "C" bool conquerd_fetch_sync(
    const char* url,
    size_t      url_len,
    char**      out_content_type,
    size_t*     out_ct_len,
    uint8_t**   out_body,
    size_t*     out_body_len
);

// ── Scheme handler ────────────────────────────────────────────────────────────
class ConquerdSchemeHandler : public QWebEngineUrlSchemeHandler
{
    Q_OBJECT
public:
    explicit ConquerdSchemeHandler(QObject* parent = nullptr)
        : QWebEngineUrlSchemeHandler(parent) {}

    void requestStarted(QWebEngineUrlRequestJob* job) override
    {
        const QUrl url = job->requestUrl();
        const QByteArray urlUtf8 = url.toString(QUrl::FullyEncoded).toUtf8();
        qInfo("[conquerd-scheme] requestStarted: %s", urlUtf8.constData());

        char*    contentType = nullptr;
        size_t   ctLen       = 0;
        uint8_t* body        = nullptr;
        size_t   bodyLen     = 0;

        const bool ok = conquerd_fetch_sync(
            urlUtf8.constData(),
            static_cast<size_t>(urlUtf8.size()),
            &contentType,
            &ctLen,
            &body,
            &bodyLen
        );

        if (!ok || body == nullptr) {
            // Serve a minimal error page so Chromium shows something useful.
            static const char errorHtml[] =
                "<!DOCTYPE html><html><head><meta charset=\"utf-8\"/>"
                "<style>body{background:#1a1a1a;color:#ccc;font-family:sans-serif;"
                "display:flex;align-items:center;justify-content:center;height:100vh;margin:0}"
                "h1{font-size:1.2rem}p{color:#8e9297;font-size:.9rem}</style></head>"
                "<body><div><h1>Portal unavailable</h1>"
                "<p>Could not reach this supernode's portal. "
                "Check your relay connection and try again.</p></div></body></html>";
            auto* buf = new QBuffer(job);
            buf->setData(QByteArray(errorHtml, static_cast<int>(sizeof(errorHtml) - 1)));
            buf->open(QIODevice::ReadOnly);
            job->reply(QByteArrayLiteral("text/html; charset=utf-8"), buf);
            if (contentType) std::free(contentType);
            return;
        }

        // Wrap body in a QByteArray (copies bytes), then free the Rust allocation.
        QByteArray bodyBytes(reinterpret_cast<const char*>(body),
                             static_cast<int>(bodyLen));
        std::free(body);

        QByteArray ct(contentType, static_cast<int>(ctLen));
        std::free(contentType);

        auto* buf = new QBuffer(job);
        buf->setData(bodyBytes);
        buf->open(QIODevice::ReadOnly);
        job->reply(ct, buf);
    }
};

#include "scheme.moc"

// ── Public C entry points ─────────────────────────────────────────────────────

extern "C" void conquerd_register_scheme()
{
    // Must be called before QCoreApplication is constructed.
    QWebEngineUrlScheme scheme("conquerd");
    // `Syntax::Host` makes Chromium treat conquerd:// as a *standard* URL
    // (like http://), which is required for relative-URL resolution to
    // preserve the authority — without this, `fetch('/foo')` from a page
    // loaded at `conquerd://PEERID/` resolves to `conquerd:///foo` and
    // drops the peer ID.  Chromium does lower-case the authority, which
    // would destroy case-sensitive base64url peer IDs — we mitigate that
    // by registering a `{lowercase_peer_id → original_peer_id}` lookup
    // table in `scheme.rs::register_portal_peer_id` whenever a portal
    // is opened.
    scheme.setSyntax(QWebEngineUrlScheme::Syntax::Host);
    scheme.setDefaultPort(QWebEngineUrlScheme::PortUnspecified);
    scheme.setFlags(
        QWebEngineUrlScheme::SecureScheme
        | QWebEngineUrlScheme::LocalAccessAllowed
        | QWebEngineUrlScheme::ServiceWorkersAllowed
        | QWebEngineUrlScheme::CorsEnabled
        | QWebEngineUrlScheme::FetchApiAllowed
    );
    QWebEngineUrlScheme::registerScheme(scheme);
}

extern "C" void conquerd_install_scheme_handler()
{
    // Must be called after QCoreApplication; installs on the default profile.
    auto* profile = QWebEngineProfile::defaultProfile();
    if (!profile) return;

    // Configure the default profile itself — every WebEngineView created
    // without an explicit `profile:` binding uses this one, which is
    // critical: only this profile carries the conquerd:// URL scheme
    // handler installed below.  If a WebEngineView were to use a
    // different profile (e.g. an inline `WebEngineProfile { ... }`),
    // navigation to a registered-but-unhandled scheme falls back to
    // QDesktopServices::openUrl() — which on Windows fires the OS-
    // registered conquerd:// handler and spawns a second ConquerD.exe.
    profile->setHttpUserAgent(QStringLiteral(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
        "AppleWebKit/537.36 (KHTML, like Gecko) "
        "Chrome/124.0.0.0 Safari/537.36"));
    profile->setSpellCheckEnabled(false);

    // Guard: don't install twice (e.g. multiple calls after hot-reload).
    if (profile->urlSchemeHandler("conquerd")) return;
    auto* handler = new ConquerdSchemeHandler(profile);
    profile->installUrlSchemeHandler("conquerd", handler);
    qInfo("[conquerd-scheme] installed handler on QWebEngineProfile::defaultProfile()=%p",
          static_cast<void*>(profile));

#ifdef CONQUERD_HAVE_QUICK_PROFILE
    // QtWebEngineQuick may surface its own default profile object to QML
    // (QQuickWebEngineProfile::defaultProfile()).  In Qt 6 it wraps the
    // same underlying ProfileAdapter, but installing explicitly here is
    // belt-and-suspenders to guarantee the handler is on whatever profile
    // the QML WebEngineView actually picks up.
    auto* qmlProfile = QQuickWebEngineProfile::defaultProfile();
    if (qmlProfile != nullptr && !qmlProfile->urlSchemeHandler("conquerd")) {
        qmlProfile->setHttpUserAgent(profile->httpUserAgent());
        qmlProfile->setSpellCheckEnabled(false);
        qmlProfile->installUrlSchemeHandler(
            "conquerd", new ConquerdSchemeHandler(qmlProfile));
        qInfo("[conquerd-scheme] installed handler on QQuickWebEngineProfile=%p",
              static_cast<void*>(qmlProfile));
    } else if (qmlProfile != nullptr) {
        qInfo("[conquerd-scheme] QQuickWebEngineProfile=%p already has handler",
              static_cast<void*>(qmlProfile));
    }
#endif

    // ── window.conquerd bridge script ──────────────────────────────────────────
    // Injected at DocumentCreation into every conquerd:// page.
    // Defines window.conquerd with:
    //   .supernodeId  — extracted from window.location.hostname (sync)
    //   .ready        — Promise that resolves with the full API object
    //       .myPeerId     our own Ed25519 public key (base64url)
    //       .version      client version string
    //       .fetch(path)  fetch() relative to the current conquerd:// origin
    //
    // Portal pages should use:  conquerd.ready.then(api => { ... })
    static const QString kBridgeJs = QStringLiteral(
        "(function(){\n"
        "  var sn = window.location.hostname;\n"
        "  var ctxUrl = 'conquerd://' + sn + '/_conquerd/ctx.json';\n"
        "  var ready = fetch(ctxUrl)\n"
        "    .then(function(r){return r.json();})\n"
        "    .then(function(ctx){\n"
        "      return Object.freeze({\n"
        "        myPeerId:    ctx.myPeerId,\n"
        "        version:     ctx.version,\n"
        "        wtBaseUrl:   ctx.wtBaseUrl  || '',\n"
        "        wtCertHash:  ctx.wtCertHash || '',\n"
        "        supernodeId: sn,\n"
        "        fetch: function(path,opts){\n"
        "          var base='conquerd://'+sn;\n"
        "          var url=path.charAt(0)==='/'?base+path:base+'/'+path;\n"
        "          return window.fetch(url,opts);\n"
        "        }\n"
        "      });\n"
        "    });\n"
        "  Object.defineProperty(window,'conquerd',{\n"
        "    configurable:false,writable:false,\n"
        "    value:Object.freeze({supernodeId:sn,ready:ready})\n"
        "  });\n"
        "})()"
    );

    QWebEngineScript bridgeScript;
    bridgeScript.setName(QStringLiteral("conquerd-bridge"));
    bridgeScript.setInjectionPoint(QWebEngineScript::DocumentCreation);
    bridgeScript.setWorldId(QWebEngineScript::MainWorld);
    bridgeScript.setRunsOnSubFrames(false);
    bridgeScript.setSourceCode(kBridgeJs);
    profile->scripts()->insert(bridgeScript);
}
