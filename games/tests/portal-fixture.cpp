#include <QtQuickTest/quicktest.h>
#include <QtWebEngineQuick/qtwebenginequickglobal.h>
#include <QtCore/QFile>
#include <QtCore/QDir>
#include <QtCore/QUrl>
#include <QtCore/QUrlQuery>
#include <QtCore/QJsonDocument>
#include <QtCore/QJsonArray>
#include <QtCore/QJsonObject>
#include <QtCore/QHash>
#include <cstdlib>
#include <cstdint>
#include <cstring>

extern "C" void conquerd_register_scheme();
extern "C" void conquerd_install_scheme_handler();
struct Peer { QString room; QJsonArray queue; };
static QHash<QString, Peer> peers;

// Real Chromium + production scheme handling; a local opaque relay replaces
// native QUIC. Each fixture hostname stands in for a separate native client.
extern "C" bool conquerd_fetch_sync(const char* raw, size_t len,
    char** contentType, size_t* ctLen, uint8_t** body, size_t* bodyLen)
{
    const QUrl url(QString::fromUtf8(raw, static_cast<qsizetype>(len)));
    const auto path = url.path();
    const auto host = url.host();
    const QUrlQuery query(url);
    QByteArray bytes, type("application/json");
    if (path == "/_conquerd/ctx.json") {
        bytes = QJsonDocument(QJsonObject{{"myPeerId",host},{"version","fixture"}}).toJson();
    } else if (path == "/_conquerd/channel/open") {
        peers[host] = Peer{query.queryItemValue("room"), {}};
        bytes = "{\"ok\":true}";
    } else if (path == "/_conquerd/channel/send") {
        const auto room = peers.value(host).room;
        for (auto i = peers.begin(); i != peers.end(); ++i) {
            if (i.key() != host && !room.isEmpty() && i->room == room) {
                if (i->queue.size() < 256) i->queue.append(query.queryItemValue("b64"));
            }
        }
        bytes = "{\"ok\":true}";
    } else if (path == "/_conquerd/channel/poll") {
        bytes = QJsonDocument(QJsonObject{{"frames", peers[host].queue}}).toJson();
        peers[host].queue = {};
    } else if (path == "/_conquerd/channel/close") {
        peers.remove(host); bytes = "{\"ok\":true}";
    } else if (path.startsWith("/games/") || path.startsWith("/web-sdk/") || path == "/") {
        auto rel = path == "/" ? QString("rust/conquerd-supernode/templates/web_index.html") : path.mid(1);
        if (rel.endsWith('/')) rel += "index.html";
        rel = QDir::cleanPath(rel);
        if (rel.startsWith("../")) return false;
        QFile file(QStringLiteral(DEMO_ROOT) + '/' + rel);
        if (!file.open(QIODevice::ReadOnly)) return false;
        bytes = file.readAll();
        type = rel.endsWith(".html") ? "text/html" : rel.endsWith(".css") ? "text/css" : "application/javascript";
    } else if (path == "/api/access/status") {
        bytes = "{\"granted\":true}";
    } else if (path.startsWith("/api/")) {
        bytes = "{\"uptime_secs\":480,\"active_rooms\":2,\"version\":\"fixture\"}";
    } else return false;
    *ctLen = static_cast<size_t>(type.size()); *bodyLen = static_cast<size_t>(bytes.size());
    *contentType = static_cast<char*>(std::malloc(*ctLen));
    *body = static_cast<uint8_t*>(std::malloc(*bodyLen));
    if (!*contentType || !*body) { std::free(*contentType); std::free(*body); *contentType = nullptr; *body = nullptr; return false; }
    std::memcpy(*contentType,type.constData(),*ctLen); std::memcpy(*body,bytes.constData(),*bodyLen);
    return true;
}
class Setup : public QObject {
    Q_OBJECT
public slots:
    void applicationAvailable() { conquerd_install_scheme_handler(); }
};
int main(int argc,char** argv) {
    conquerd_register_scheme(); QtWebEngineQuick::initialize(); Setup setup;
    return quick_test_main_with_setup(argc,argv,"portal_apps",nullptr,&setup);
}
#include "portal-fixture.moc"
