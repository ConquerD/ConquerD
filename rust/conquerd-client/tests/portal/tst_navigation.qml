import QtQuick
import QtTest
import ConquerD.Client 1.0

Item {
    width: 640
    height: 360

    Component {
        id: portalComponent
        ConquerdWebView {
            width: 640
            height: 360
            allowConquerd: true
        }
    }

    TestCase {
        name: "PortalNavigation"
        when: windowShown

        function test_load_data() {
            return [
                { tag: "start", initial: true, url: "d://grd123/", expected: "conquerd://grd123/" },
                { tag: "navigate", initial: false, url: "d://grd123/access.html", expected: "conquerd://grd123/access.html" },
                { tag: "uppercase", initial: false, url: "D://grd123/games/?mode=test#play", expected: "conquerd://grd123/games/?mode=test#play" },
                { tag: "legacy", initial: true, url: "conquerd://grd123/", expected: "conquerd://grd123/" }
            ]
        }

        function test_load(data) {
            var portal = createTemporaryObject(portalComponent, parent,
                { startUrl: data.initial ? data.url : "" })
            verify(portal !== null)
            if (!data.initial)
                portal.navigate(data.url)
            // The page title is set only after a root-relative fetch succeeds.
            // This checks the loaded Chromium document, not just QML's url
            // property (which retains d:// even when Chromium opens file:).
            tryCompare(portal, "pageTitle", data.expected, 10000)
            verify(!portal._errorVisible)
        }

        function test_relativeNavigation() {
            var portal = createTemporaryObject(portalComponent, parent,
                { startUrl: "d://grd123/" })
            tryCompare(portal, "pageTitle", "conquerd://grd123/", 10000)
            // The fixture's only link is at the page's default top-left margin.
            mouseClick(portal, 25, 15)
            tryCompare(portal, "pageTitle", "conquerd://grd123/games/demo/?mode=test#play", 10000)
        }

        function test_urlPolicy() {
            var portal = createTemporaryObject(portalComponent, parent)
            verify(portal.isPortalUrl("d://grd123/"))
            verify(portal.isPortalUrl("D://grd123/"))
            verify(portal.isPortalUrl("conquerd://grd123/"))
            verify(!portal.isPortalUrl("https://example.com/"))
            compare(portal.browserUrl("d://AbC_-123/games/?q=a%20b#play"),
                    "conquerd://AbC_-123/games/?q=a%20b#play")
            compare(portal.browserUrl("https://example.com/"), "https://example.com/")
            compare(portal.browserUrl("file:///D:/notes.txt"), "file:///D:/notes.txt")
        }
    }
}
