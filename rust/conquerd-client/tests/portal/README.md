These tests load the production `ConquerdWebView.qml` in Qt WebEngine with the
production scheme handler. Only the Rust QUIC fetch callback is replaced with
local HTML/JSON fixtures. No identity, running client, or supernode is needed.

Run on Windows with Qt 6.8+ and the Visual Studio C++ tools installed, from the
repository root:

```powershell
cmake -S rust/conquerd-client/tests/portal -B .tmp-run/portal-build -G "Visual Studio 17 2022" -A x64 -DCMAKE_PREFIX_PATH=C:/Qt/6.8.3/msvc2022_64
cmake --build .tmp-run/portal-build --config Release
$env:PATH = "C:/Qt/6.8.3/msvc2022_64/bin;$env:PATH"
ctest --test-dir .tmp-run/portal-build -C Release --output-on-failure
```

Chromium must be able to launch its renderer subprocess; an agent execution
sandbox can prevent this. Run the test outside that sandbox if page loads stall.

The tests check the loaded document and a root-relative fetch, since QML's `url`
property retains `d://` even when Windows Chromium rewrites the actual request
to `file:///D://`. Desktop navigation uses `conquerd://` internally to avoid that
drive-letter conversion. The public `d://` scheme remains accepted.
