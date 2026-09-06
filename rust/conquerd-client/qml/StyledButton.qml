import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Button {
    id: control

    property bool primary: false
    property bool danger: false
    property bool success: false
    property bool compact: true
    readonly property bool hasIcon: String(icon.source).length > 0
    readonly property bool filled: primary || danger || success
    readonly property color baseColor: danger ? Theme.danger
        : success ? Theme.online
        : primary ? Theme.accent
        : flat ? "transparent"
        : Theme.bg3

    implicitWidth: Math.max(
        Theme.touchTarget,
        buttonContent.implicitWidth
            + leftPadding
            + rightPadding
    )
    implicitHeight: compact ? Theme.controlHeight : Theme.touchTarget
    topInset: 0
    bottomInset: 0
    // Material.Button sets verticalPadding to 14 (Normal) / 10 (Dense).
    // That leftover padding is what parks icon+label in the bottom of a
    // 32px control (and nearly fills the 44px title bar if the layout
    // lets the button grow). Zero it so contentItem uses the full height.
    padding: 0
    verticalPadding: 0
    leftPadding: Theme.spacingMd
    rightPadding: Theme.spacingMd
    font.pixelSize: Theme.fontSizeBody
    flat: !filled

    icon.width: 20
    icon.height: 20
    icon.color: enabled
        ? (filled ? Theme.textInv : Theme.text)
        : Theme.muted

    contentItem: Item {
        implicitWidth: buttonContent.implicitWidth
        implicitHeight: Math.max(labelText.implicitHeight, control.hasIcon ? control.icon.height : 0)

        RowLayout {
            id: buttonContent
            anchors.fill: parent
            spacing: control.hasIcon && control.text.length > 0 ? Theme.spacingSm : 0

            Image {
                visible: control.hasIcon
                source: control.icon.source
                sourceSize.width: control.icon.width
                sourceSize.height: control.icon.height
                width: control.icon.width
                height: control.icon.height
                Layout.preferredWidth: control.icon.width
                Layout.preferredHeight: control.icon.height
                Layout.alignment: Qt.AlignCenter
                fillMode: Image.PreserveAspectFit
                opacity: control.enabled ? 1.0 : 0.5
            }

            Text {
                id: labelText
                visible: control.text.length > 0
                text: control.text
                color: control.enabled
                    ? (control.filled ? Theme.textInv : Theme.text)
                    : Theme.muted
                font: control.font
                Layout.fillWidth: true
                Layout.minimumWidth: 0
                Layout.alignment: Qt.AlignVCenter
                elide: Text.ElideRight
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }
        }
    }

    Accessible.role: Accessible.Button
    Accessible.name: text

    background: Rectangle {
        radius: Theme.radiusMd
        color: !control.enabled ? Theme.bg2
            : control.pressed ? (control.flat ? Theme.selectedFill() : Qt.darker(control.baseColor, 1.18))
            : control.hovered ? (control.flat ? Theme.bg3 : Qt.lighter(control.baseColor, 1.08))
            : control.baseColor
        border.width: control.visualFocus ? 2 : (control.flat ? 0 : 1)
        border.color: control.visualFocus ? Theme.text : (control.filled ? control.baseColor : Theme.divider)

        Behavior on color { ColorAnimation { duration: Theme.animFast } }
        Behavior on border.color { ColorAnimation { duration: Theme.animFast } }
    }
}
