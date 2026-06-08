import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Label {
    Layout.fillWidth: true
    color: Theme.muted
    font.pixelSize: Theme.fontSizeCaption
    font.bold: true
    font.letterSpacing: 1.2
    text: title.toUpperCase()

    property string title: ""
}
