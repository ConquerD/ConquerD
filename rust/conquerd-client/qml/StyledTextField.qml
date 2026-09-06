import QtQuick
import QtQuick.Controls.Material
import ConquerD.Client 1.0

TextField {
    id: control

    color: enabled ? Theme.text : Theme.muted
    placeholderTextColor: Theme.muted
    selectionColor: Theme.accent
    selectedTextColor: Theme.textInv
    hoverEnabled: true
    font.pixelSize: Theme.fontSizeBody
    leftPadding: Theme.spacingMd
    rightPadding: Theme.spacingMd
    implicitHeight: Theme.controlHeight

    background: Rectangle {
        radius: Theme.radiusMd
        color: control.enabled ? Theme.bg2 : Theme.bg1
        border.width: control.activeFocus ? 2 : 1
        border.color: !control.enabled ? Theme.border
            : control.activeFocus ? Theme.accent
            : control.hovered ? Theme.muted : Theme.divider

        Behavior on border.color { ColorAnimation { duration: Theme.animFast } }
    }
}