import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: card

    property string title: ""
    property string subtitle: ""
    default property alias content: body.data

    Layout.fillWidth: true
    radius: Theme.radiusMd
    color: Theme.bg2
    border.color: Theme.bg3
    border.width: 1
    implicitHeight: body.implicitHeight + Theme.spacingLg * 2

    ColumnLayout {
        id: body
        x: Theme.spacingLg
        y: Theme.spacingLg
        width: Math.max(0, card.width - Theme.spacingLg * 2)
        spacing: Theme.spacingMd

        ColumnLayout {
            visible: card.title.length > 0 || card.subtitle.length > 0
            Layout.fillWidth: true
            spacing: Theme.spacingXs

            Label {
                visible: card.title.length > 0
                text: card.title
                color: Theme.text
                font.pixelSize: Theme.fontSizeTitle
                font.bold: true
            }

            Label {
                visible: card.subtitle.length > 0
                Layout.fillWidth: true
                text: card.subtitle
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                wrapMode: Text.WordWrap
            }
        }
    }
}
