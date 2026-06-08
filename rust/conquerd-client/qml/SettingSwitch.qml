import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    property string title: ""
    property string description: ""
    property bool checked: false
    property bool enabledState: true
    signal changed(bool checked)

    Layout.fillWidth: true
    implicitHeight: Math.max(Theme.touchTarget, row.implicitHeight)
    enabled: enabledState
    opacity: enabled ? 1.0 : 0.5
    Accessible.role: Accessible.CheckBox
    Accessible.name: root.title
    Accessible.description: root.description

    RowLayout {
        id: row
        anchors.fill: parent
        spacing: Theme.spacingMd

        ColumnLayout {
            Layout.fillWidth: true
            spacing: Theme.spacingXs

            Label {
                Layout.fillWidth: true
                text: root.title
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody
                elide: Text.ElideRight
            }

            Label {
                visible: root.description.length > 0
                Layout.fillWidth: true
                text: root.description
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                wrapMode: Text.WordWrap
            }
        }

        Switch {
            checked: root.checked
            enabled: root.enabled
            Layout.alignment: Qt.AlignVCenter
            onToggled: root.changed(checked)
        }
    }

    MouseArea {
        anchors.fill: parent
        enabled: root.enabled
        cursorShape: Qt.PointingHandCursor
        onClicked: root.changed(!root.checked)
    }
}
