import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: root

    property int currentIndex: 0
    signal sectionActivated(int index)
    signal saveRequested()

    Layout.preferredWidth: Theme.sidebarWidth
    Layout.fillHeight: true
    color: Theme.bg1

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            height: Theme.touchTarget
            color: Theme.bg2

            Label {
                anchors.centerIn: parent
                text: "Settings"
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody
                font.bold: true
            }
        }

        ListView {
            id: navList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: ListModel {
                ListElement { label: "Audio"; icon: "qrc:/qt/qml/ConquerD/Client/icons/headphone.svg" }
                ListElement { label: "Video"; icon: "qrc:/qt/qml/ConquerD/Client/icons/video.svg" }
                ListElement { label: "Identity"; icon: "qrc:/qt/qml/ConquerD/Client/icons/person.svg" }
                ListElement { label: "General"; icon: "qrc:/qt/qml/ConquerD/Client/icons/gear.svg" }
                ListElement { label: "AI"; icon: "qrc:/qt/qml/ConquerD/Client/icons/lightning.svg" }
                ListElement { label: "Network"; icon: "qrc:/qt/qml/ConquerD/Client/icons/globe.svg" }
                ListElement { label: "Security"; icon: "qrc:/qt/qml/ConquerD/Client/icons/lock.svg" }
                ListElement { label: "Privacy"; icon: "qrc:/qt/qml/ConquerD/Client/icons/key.svg" }
                ListElement { label: "Diagnostics"; icon: "qrc:/qt/qml/ConquerD/Client/icons/logs.svg" }
            }

            delegate: Item {
                id: navItem
                width: navList.width
                height: Theme.touchTarget
                property bool highlighted: index === root.currentIndex
                property bool hovered: navMouse.containsMouse

                Rectangle {
                    anchors.fill: parent
                    color: navItem.hovered ? Theme.bg3 : "transparent"

                    Rectangle {
                        anchors.fill: parent
                        color: Theme.selectedFill()
                        visible: navItem.highlighted
                    }

                    Rectangle {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: 3
                        height: parent.height - Theme.spacingMd
                        radius: Theme.radiusSm
                        color: Theme.accent
                        visible: navItem.highlighted
                    }

                    Behavior on color { ColorAnimation { duration: Theme.animFast } }
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: Theme.spacingLg
                    anchors.rightMargin: Theme.spacingSm
                    spacing: Theme.spacingSm

                    Image {
                        source: model.icon
                        sourceSize.width: 20
                        sourceSize.height: 20
                        Layout.preferredWidth: 20
                        Layout.preferredHeight: 20
                        fillMode: Image.PreserveAspectFit
                        opacity: navItem.highlighted ? 1.0 : 0.72
                    }

                    Label {
                        Layout.fillWidth: true
                        text: model.label
                        color: navItem.highlighted ? Theme.text : Theme.muted
                        font.pixelSize: Theme.fontSizeBody
                        font.bold: navItem.highlighted
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }

                MouseArea {
                    id: navMouse
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.sectionActivated(index)
                }

                Accessible.role: Accessible.Button
                Accessible.name: model.label
            }
        }

        Rectangle {
            Layout.fillWidth: true
            height: 1
            color: Theme.bg3
        }

        Rectangle {
            Layout.fillWidth: true
            height: 52
            color: Theme.bg0

            StyledButton {
                anchors.centerIn: parent
                text: "Save Settings"
                primary: true
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/save.svg"
                onClicked: root.saveRequested()
            }
        }
    }
}
