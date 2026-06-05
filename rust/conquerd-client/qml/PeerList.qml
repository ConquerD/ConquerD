// PeerList.qml — Navigation rail showing connected peers and rooms.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: root
    color: Theme.bg1

    property int peerCount: 0
    property var peerModel: null
    signal peerSelected(string peerId, string handle)
    signal startCallRequested(string peerId)
    signal removePeerRequested(string peerId)
    signal copyPeerIdRequested(string peerId)
    signal blockPeerRequested(string peerId)
    signal unblockPeerRequested(string peerId)
    signal clearHistoryRequested(string peerId)

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Header
        Rectangle {
            Layout.fillWidth: true
            height: 36
            color: Theme.bg2

            Text {
                anchors {
                    verticalCenter: parent.verticalCenter
                    left: parent.left
                    leftMargin: Theme.spacingMd
                }
                text: "Peers (" + root.peerCount + ")"
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                font.capitalization: Font.AllUppercase
                font.letterSpacing: 1.2
                font.bold: true
            }
        }

        // Peer list backed by PeerListModel — with section grouping by online status
        ListView {
            id: peerListView
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: root.peerModel

            // Section grouping: Online peers first, then Offline
            section.property: "online"
            section.criteria: ViewSection.FullString
            section.delegate: Rectangle {
                width: peerListView.width
                height: 22
                color: "transparent"

                Text {
                    anchors {
                        verticalCenter: parent.verticalCenter
                        left: parent.left
                        leftMargin: Theme.spacingMd
                    }
                    text: (section === "true" ? "Online" : "Offline").toUpperCase()
                    color: Theme.muted
                    font.pixelSize: 10
                    font.capitalization: Font.AllUppercase
                    font.letterSpacing: 0.8
                    font.bold: true
                }
            }

            // Empty state
            ColumnLayout {
                anchors.centerIn: parent
                visible: root.peerCount === 0
                width: Math.min(parent.width - 32, 170)
                spacing: 8

                Image {
                    source: "qrc:/qt/qml/ConquerD/Client/icons/peers.svg"
                    sourceSize.width: 32
                    sourceSize.height: 32
                    Layout.preferredWidth: 32
                    Layout.preferredHeight: 32
                    Layout.alignment: Qt.AlignHCenter
                    fillMode: Image.PreserveAspectFit
                    opacity: 0.72
                }

                Text {
                    text: "No peers yet"
                    horizontalAlignment: Text.AlignHCenter
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeBody
                    font.bold: true
                    Layout.fillWidth: true
                }

                Text {
                    text: "Paste an invite above to add a trusted peer."
                    horizontalAlignment: Text.AlignHCenter
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeCaption
                    wrapMode: Text.WordWrap
                    lineHeight: 1.3
                    Layout.fillWidth: true
                }
            }

            delegate: Rectangle {
                id: delegateItem
                width: ListView.view.width
                height: 56
                color: mouseArea.containsMouse ? Theme.bg3 : "transparent"

                required property string peerId
                required property string handle
                required property bool online
                required property bool inCall
                required property int unreadCount
                required property string lastPreview
                required property bool isTyping
                required property bool blocked

                // 3px left accent bar (visible when in call or unread)
                Rectangle {
                    visible: delegateItem.unreadCount > 0 || delegateItem.inCall
                    width: 3
                    anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
                    color: delegateItem.inCall ? Theme.online : Theme.accent
                    radius: 0
                }

                RowLayout {
                    anchors {
                        fill: parent
                        leftMargin: Theme.spacingMd
                        rightMargin: Theme.spacingSm
                        topMargin: Theme.spacingXs
                        bottomMargin: Theme.spacingXs
                    }
                    spacing: Theme.spacingSm

                    // Identity-derived avatar with status ring
                    // (red = blocked, green = online, grey = offline)
                    Avatar {
                        id: peerAvatar
                        peerId: delegateItem.peerId
                        size: 36
                        showRing: true
                        ringColor: delegateItem.blocked ? Theme.danger
                                 : delegateItem.online   ? Theme.online
                                 : peerAvatar.tintColor
                        Layout.alignment: Qt.AlignVCenter

                        ToolTip.visible: delegateItem.blocked && hoverHandler.hovered
                        ToolTip.text: "Blocked"
                        HoverHandler { id: hoverHandler }
                    }

                    // Name + preview column
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 2

                        // Display name row
                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 4

                            Text {
                                text: delegateItem.handle || delegateItem.peerId || ""
                                color: Theme.text
                                font.pixelSize: Theme.fontSizeBody
                                font.bold: delegateItem.unreadCount > 0
                                elide: Text.ElideRight
                                Layout.fillWidth: true
                            }

                            // In-call phone icon
                            Image {
                                visible: delegateItem.inCall
                                source: "qrc:/qt/qml/ConquerD/Client/icons/phone.svg"
                                sourceSize.width: 12
                                sourceSize.height: 12
                                width: 12
                                height: 12
                                fillMode: Image.PreserveAspectFit
                            }
                        }

                        // Preview / typing row
                        Text {
                            visible: delegateItem.isTyping || delegateItem.lastPreview !== ""
                            text: delegateItem.isTyping
                                ? "typing\u2026"
                                : delegateItem.lastPreview
                            color: delegateItem.isTyping ? Theme.accent : Theme.muted
                            font.pixelSize: Theme.fontSizeCaption
                            font.italic: delegateItem.isTyping
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                    }

                    // Unread badge
                    Rectangle {
                        visible: delegateItem.unreadCount > 0
                        width: Math.max(20, badgeText.implicitWidth + 8)
                        height: 20
                        radius: 10
                        color: Theme.danger
                        Layout.alignment: Qt.AlignVCenter

                        Text {
                            id: badgeText
                            anchors.centerIn: parent
                            text: delegateItem.unreadCount > 99
                                ? "99+"
                                : delegateItem.unreadCount.toString()
                            color: "#ffffff"
                            font.pixelSize: 10
                            font.bold: true
                        }
                    }
                }

                MouseArea {
                    id: mouseArea
                    anchors.fill: parent
                    hoverEnabled: true
                    acceptedButtons: Qt.LeftButton | Qt.RightButton

                    onClicked: function(mouse) {
                        if (mouse.button === Qt.LeftButton) {
                            root.peerSelected(delegateItem.peerId, delegateItem.handle || delegateItem.peerId)
                        } else if (mouse.button === Qt.RightButton) {
                            peerContextMenu.targetPeerId = delegateItem.peerId
                            peerContextMenu.targetHandle = delegateItem.handle || delegateItem.peerId
                            peerContextMenu.targetBlocked = delegateItem.blocked
                            peerContextMenu.popup()
                        }
                    }
                }
            }
        }
    }

    // ── Per-peer context menu ─────────────────────────────────────────────
    Menu {
        id: peerContextMenu
        property string targetPeerId: ""
        property string targetHandle: ""
        property bool targetBlocked: false

        MenuItem {
            text: "Start Call"
            onTriggered: root.startCallRequested(peerContextMenu.targetPeerId)
        }

        MenuItem {
            text: "Copy Peer ID"
            onTriggered: root.copyPeerIdRequested(peerContextMenu.targetPeerId)
        }

        MenuSeparator {}

        MenuItem {
            text: "Remove Peer"
            onTriggered: root.removePeerRequested(peerContextMenu.targetPeerId)
        }

        MenuItem {
            text: peerContextMenu.targetBlocked ? "Unblock Peer" : "Block Peer"
            onTriggered: {
                if (peerContextMenu.targetBlocked)
                    root.unblockPeerRequested(peerContextMenu.targetPeerId)
                else
                    root.blockPeerRequested(peerContextMenu.targetPeerId)
            }
        }

        MenuSeparator {}

        MenuItem {
            text: "Clear Chat History"
            onTriggered: root.clearHistoryRequested(peerContextMenu.targetPeerId)
        }
    }
}

