import QtQuick
import QtTest
import QtWebEngine

Item {
    id: root
    width: 1200
    height: 850
    Component { id: browser; WebEngineView { width: 1100; height: 780 } }
    TestCase {
        name: "PortalApps"
        when: windowShown
        function js(view, source) {
            var done = false, result
            view.runJavaScript(source, function(value) { result = value; done = true })
            tryVerify(function() { return done }, 5000)
            return result
        }
        function open(host, app) {
            var view = createTemporaryObject(browser, root)
            view.url = "conquerd://" + host + "/games/" + app + "/?room=fixture"
            tryVerify(function() { return js(view, "document.querySelector('#status')?.className") === "connected" }, 15000)
            return view
        }
        function test_layout_data() {
            return [ { tag:"brick", app:"brick-breaker" }, { tag:"presence", app:"example" }, { tag:"canvas", app:"shared-drawing" } ]
        }
        function test_layout(data) {
            var view = open("layout-" + data.tag, data.app)
            verify(js(view, "!document.querySelector('#overlay')"))
            verify(js(view, "document.querySelector('.session-panel').hidden"))
            verify(js(view, "(() => {const c=document.querySelector('canvas').getBoundingClientRect(),t=document.querySelector('.toolbar').getBoundingClientRect();return c.top>=t.bottom && c.bottom<=innerHeight && c.width>100 && c.height>100})()"))
            js(view, "document.querySelector('#session-toggle').click()")
            verify(js(view, "(() => {const c=document.querySelector('canvas').getBoundingClientRect(),p=document.querySelector('.session-panel').getBoundingClientRect();return c.right<=p.left})()"))
            js(view, "document.querySelector('#focus').click()")
            verify(js(view, "document.querySelector('.app').classList.contains('focus-mode')"))
            js(view, "document.querySelector('#exit-focus').click()")
            view.width = 390; view.height = 700
            wait(150)
            verify(js(view, "document.querySelector('.session-panel').getBoundingClientRect().width<=innerWidth"))
            js(view, "document.querySelector('#session-toggle').click()")
            wait(150)
            verify(js(view, "document.documentElement.scrollWidth<=innerWidth"))
            verify(js(view, "document.querySelector('canvas').getBoundingClientRect().height>100"))
        }
        function test_multiplayerBrick() {
            var a = open("brick-a", "brick-breaker"), b = open("brick-b", "brick-breaker")
            tryVerify(function() { return js(a,"document.querySelector('#peers-count').textContent") === "2 here" && js(b,"document.querySelector('#peers-count').textContent") === "2 here" },10000)
            wait(1500)
            js(a,"document.querySelector('#ready').click()")
            tryVerify(function() { return js(b,"document.querySelector('.members').textContent.includes('Ready')") },5000)
            js(a,"document.querySelector('#launch').click()")
            tryVerify(function() { return js(a,"document.querySelector('#launch').disabled") && js(b,"document.querySelector('#launch').disabled") },5000)
            js(b,"document.querySelector('#reset').click()")
            tryVerify(function() { return !js(a,"document.querySelector('#launch').disabled") && !js(b,"document.querySelector('#launch').disabled") },5000)
            // Closing either peer must leave a usable single-player simulation.
            a.url = "about:blank"
            tryVerify(function() { return js(b,"document.querySelector('#peers-count').textContent") === "1 here" },10000)
            js(b,"document.querySelector('#launch').click()")
            tryVerify(function() { return js(b,"document.querySelector('#launch').disabled") },5000)
        }
        function test_drawingLateJoin() {
            var a=open("draw-a","shared-drawing")
            mousePress(a,300,300);mouseMove(a,420,360,150);mouseRelease(a,420,360)
            tryVerify(function() { return js(a,"/[1-9][0-9]* shared operations/.test(document.querySelector('#notice').textContent)") },5000)
            var b=open("draw-b","shared-drawing")
            tryVerify(function() { return js(a,"document.querySelector('#notice').textContent") === js(b,"document.querySelector('#notice').textContent") },12000)
            verify(js(b,"/[1-9][0-9]* shared operations/.test(document.querySelector('#notice').textContent)"))
        }
        function test_portalLaunchpad() {
            var view=createTemporaryObject(browser,root)
            view.url="conquerd://hub/"
            tryVerify(function(){return js(view,"document.querySelector('#main')?.classList.contains('visible')")},10000)
            compare(js(view,"document.querySelectorAll('.game-card').length"),3)
            js(view,"document.querySelector('#demo-room').value='Friday night';document.querySelector('#demo-room').dispatchEvent(new Event('input'))")
            verify(js(view,"document.querySelector('#game-brick').href.includes('room=Friday%20night')"))
        }
    }
}
