// PeerVolumePopup.qml — per-peer playback volume for this listener.
//
// A separate popup rather than a Slider embedded in the context Menu: a live
// slider inside a Quick Controls Menu dismisses the menu on click-through under
// some styles, which makes it impossible to drag. Less elegant, reliably usable.

import QtQuick
import QtQuick.Controls
import ConquerD.Client 1.0

Popup {
    id: root

    /// Peer this popup is adjusting.
    property string peerId: ""
    /// Display name for the header.
    property string peerName: ""
    /// Current volume percentage (100 = unity).
    property int volumePct: 100

    /// Emitted live while dragging so the change is audible immediately.
    signal volumeChanged(string peerId, int pct)

    /// Open for `pid`, seeded with its current value.
    function openFor(pid, name, pct) {
        root.peerId = pid
        root.peerName = name
        root.volumePct = pct
        slider.value = pct
        root.open()
    }

    modal: false
    focus: true
    padding: Theme.spacingMd
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutsideParent

    background: Rectangle {
        color: Theme.bg3
        border.color: Theme.divider
        border.width: 1
        radius: Theme.radiusSm
    }

    contentItem: Column {
        spacing: Theme.spacingSm

        Text {
            width: 200
            text: root.peerName || root.peerId
            color: Theme.text
            font.pixelSize: Theme.fontSizeBody
            font.bold: true
            elide: Text.ElideRight
        }

        Row {
            spacing: Theme.spacingSm

            Slider {
                id: slider
                width: 150
                from: 0
                to: 200
                stepSize: 5
                value: root.volumePct
                // Live rather than on-release: hearing the change while
                // dragging is the whole point of a volume control.
                onMoved: {
                    root.volumePct = Math.round(value)
                    root.volumeChanged(root.peerId, root.volumePct)
                }
            }

            Text {
                anchors.verticalCenter: parent.verticalCenter
                width: 40
                text: root.volumePct + "%"
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                horizontalAlignment: Text.AlignRight
            }
        }

        Text {
            visible: root.volumePct === 0
            text: qsTr("Silenced for you")
            color: Theme.danger
            font.pixelSize: Theme.fontSizeCaption
        }
    }
}
