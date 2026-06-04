// app_icon.cpp — sets the QGuiApplication window icon from the embedded
// QRC resource so that the Windows taskbar, alt-tab switcher, and title
// bar all show the ConquerD logo.  Qt does not automatically promote the
// PE VERSIONINFO icon (resource ID 1) to QGuiApplication::windowIcon().
//
// Called once from main.rs immediately after QGuiApplication::new().

#include <QGuiApplication>
#include <QIcon>

extern "C" void conquerd_set_app_icon() {
    QGuiApplication::setWindowIcon(QIcon(":/assets/conquerd.ico"));
}
