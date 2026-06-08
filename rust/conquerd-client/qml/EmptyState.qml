import QtQuick
import QtQuick.Layouts
import ConquerD.Client 1.0

ColumnLayout {
    id: root

    property string iconSource: ""
    property int iconSize: 32
    property string title: ""
    property string subtitle: ""

    spacing: Theme.spacingSm

    visible: title.length > 0 || subtitle.length > 0

    Image {
        visible: root.iconSource !== ""
        source: root.iconSource
        sourceSize.width: root.iconSize
        sourceSize.height: root.iconSize
        Layout.preferredWidth: root.iconSize
        Layout.preferredHeight: root.iconSize
        Layout.alignment: Qt.AlignHCenter
        fillMode: Image.PreserveAspectFit
        opacity: 0.72
    }

    Text {
        visible: root.title !== ""
        text: root.title
        horizontalAlignment: Text.AlignHCenter
        color: Theme.text
        font.pixelSize: Theme.fontSizeBody
        font.bold: true
        Layout.fillWidth: true
    }

    Text {
        visible: root.subtitle !== ""
        text: root.subtitle
        horizontalAlignment: Text.AlignHCenter
        color: Theme.muted
        font.pixelSize: Theme.fontSizeCaption
        wrapMode: Text.WordWrap
        lineHeight: 1.3
        Layout.fillWidth: true
    }
}