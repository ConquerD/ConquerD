#include <QtQuickTest/quicktest.h>
#include <QtWebEngineQuick/qtwebenginequickglobal.h>
#include <QtCore/QUrl>
#include <cstdint>
#include <cstdlib>
#include <cstring>

extern "C" void conquerd_register_scheme();
extern "C" void conquerd_install_scheme_handler();

// Replace only the QUIC fetch boundary. Production scheme registration,
// Chromium navigation, relative URL resolution, and QML run unchanged.
extern "C" bool conquerd_fetch_sync(
    const char* url, size_t url_len, char** content_type, size_t* ct_len,
    uint8_t** body, size_t* body_len)
{
    const QUrl request(QString::fromUtf8(url, static_cast<qsizetype>(url_len)));
    const bool json = request.path().endsWith(QStringLiteral(".json"));
    const QByteArray type = json ? "application/json" : "text/html";
    const QByteArray bytes = json ? QByteArray("{\"myPeerId\":\"test-peer\"}") : QByteArray(
        "<!doctype html><title>Loading</title>"
        "<a id='relative' href='/games/demo/?mode=test#play'>Game</a>"
        "<script>fetch('/probe.json').then(r=>r.json()).then(() => {"
        "document.title = location.protocol + '//' + location.host + "
        "location.pathname + location.search + location.hash;"
        "});</script>");
    *ct_len = static_cast<size_t>(type.size());
    *body_len = static_cast<size_t>(bytes.size());
    *content_type = static_cast<char*>(std::malloc(*ct_len));
    *body = static_cast<uint8_t*>(std::malloc(*body_len));
    if (!*content_type || !*body) {
        std::free(*content_type);
        std::free(*body);
        *content_type = nullptr;
        *body = nullptr;
        return false;
    }
    std::memcpy(*content_type, type.constData(), *ct_len);
    std::memcpy(*body, bytes.constData(), *body_len);
    return true;
}

class PortalSetup : public QObject
{
    Q_OBJECT
public slots:
    void applicationAvailable() { conquerd_install_scheme_handler(); }
};

int main(int argc, char** argv)
{
    conquerd_register_scheme();
    QtWebEngineQuick::initialize();
    PortalSetup setup;
    return quick_test_main_with_setup(argc, argv, "portal_navigation", nullptr, &setup);
}

#include "main.moc"
