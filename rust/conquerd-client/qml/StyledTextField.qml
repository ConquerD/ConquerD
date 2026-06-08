import QtQuick
import QtQuick.Controls.Material
import ConquerD.Client 1.0

TextField {
    id: control

    color: Theme.text
    placeholderTextColor: Theme.muted
    font.pixelSize: Theme.fontSizeBody
    leftPadding: Theme.spacingSm
    rightPadding: Theme.spacingSm
    implicitHeight: Theme.controlHeight

    background: Rectangle {
        radius: Theme.radiusMd
        color: Theme.bg3
        border.width: 1
        border.color: control.activeFocus ? Theme.accent : Theme.bg3

        Behavior on border.color { ColorAnimation { duration: Theme.animFast } }
    }
}