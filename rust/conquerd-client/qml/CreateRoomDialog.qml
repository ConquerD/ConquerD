// CreateRoomDialog.qml — Dialog to create or join an SFU audio room.
//
// Accepts a room name + supernode selection and fires createRoom on the bridge.
// Open via: createRoomDialog.open()

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Dialog {
    id: root

    // The live node list model from MainWindow
    required property ListModel nodeListModel

    title: "Create Room"
    modal: true
    standardButtons: Dialog.Ok | Dialog.Cancel
    closePolicy: Dialog.CloseOnEscape

    // Reset fields each time the dialog opens
    onOpened: {
        roomNameField.text  = ""
        supernodeBox.currentIndex = 0
        roomNameField.forceActiveFocus()
    }

    onAccepted: {
        var name = roomNameField.text.trim()
        if (name === "" || supernodeBox.currentIndex < 0) return
        var snId = nodeListModel.get(supernodeBox.currentIndex).nodeId || ""
        if (snId === "") return
        backend.createRoom(snId, name)
    }

    background: Rectangle {
        color: Theme.bg1
        radius: 0
        border.color: Theme.bg3
        border.width: 1
    }

    header: Rectangle {
        color: Theme.bg0
        height: 48
        radius: 0
        Text {
            anchors.centerIn: parent
            text: root.title
            color: Theme.text
            font.pixelSize: Theme.fontSizeBody
            font.bold: true
        }
    }

    contentItem: ColumnLayout {
        spacing: 16

        // Room name
        ColumnLayout {
            spacing: 4
            Layout.fillWidth: true

            Text {
                text: "Room name"
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
            }

            TextField {
                id: roomNameField
                Layout.fillWidth: true
                placeholderText: "e.g. Game Night"
                maximumLength: 64
                background: Rectangle {
                    color: Theme.bg2
                    radius: 0
                    border.color: roomNameField.activeFocus ? Theme.accent : Theme.bg3
                    border.width: 1
                }
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody
                Keys.onReturnPressed: root.accept()
            }
        }

        // Supernode selection
        ColumnLayout {
            spacing: 4
            Layout.fillWidth: true

            Text {
                text: "Host supernode"
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
            }

            ComboBox {
                id: supernodeBox
                Layout.fillWidth: true
                model: root.nodeListModel
                textRole: "displayName"

                background: Rectangle {
                    color: Theme.bg2
                    radius: 0
                    border.color: supernodeBox.activeFocus ? Theme.accent : Theme.bg3
                    border.width: 1
                }

                contentItem: Text {
                    leftPadding: 8
                    text: supernodeBox.displayText
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeBody
                    verticalAlignment: Text.AlignVCenter
                }

                delegate: ItemDelegate {
                    required property int index
                    required property string displayName
                    required property string nodeId
                    width: supernodeBox.width
                    contentItem: Text {
                        text: displayName || nodeId
                        color: Theme.text
                        font.pixelSize: Theme.fontSizeBody
                    }
                    highlighted: supernodeBox.highlightedIndex === index
                    background: Rectangle {
                        color: highlighted ? Theme.accent : Theme.bg2
                    }
                }

                // Shown when no nodes are connected
                popup: Popup {
                    y: supernodeBox.height + 2
                    width: supernodeBox.width
                    implicitHeight: contentItem.implicitHeight
                    padding: 0

                    contentItem: ListView {
                        clip: true
                        implicitHeight: contentHeight
                        model: supernodeBox.delegateModel
                        ScrollBar.vertical: ScrollBar {}
                    }

                    background: Rectangle {
                        color: Theme.bg2
                        radius: 0
                        border.color: Theme.bg3
                        border.width: 1
                    }
                }
            }

            Text {
                visible: root.nodeListModel.count === 0
                text: "No supernodes connected. Connect to a supernode first."
                color: Theme.danger
                font.pixelSize: Theme.fontSizeCaption
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
        }
    }

    // Override OK button to enforce validation
    footer: DialogButtonBox {
        standardButtons: root.standardButtons
        background: Rectangle {
            color: Theme.bg0
            radius: 0
            Rectangle {
                anchors { left: parent.left; right: parent.right; top: parent.top }
                height: 10; color: Theme.bg0
            }
        }
        delegate: Button {
            text: DialogButtonBox.buttonText(this)
            enabled: DialogButtonBox.buttonRole(this) !== DialogButtonBox.AcceptRole
                  || (roomNameField.text.trim() !== "" && supernodeBox.currentIndex >= 0 && root.nodeListModel.count > 0)
        }
    }
}
