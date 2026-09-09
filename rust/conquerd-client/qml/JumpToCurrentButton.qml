import QtQuick
import ConquerD.Client 1.0

/*!
    A "jump to current" affordance for a chat \l ListView.

    Appears once the view has been scrolled far enough from the newest message
    that an arriving message would land unseen below the fold, and returns to
    the latest message when clicked.

    Purely an affordance: the panel decides for itself whether to keep
    auto-scrolling. This only reports that the reader has scrolled away and
    offers a way back, so reading history is never interrupted by a message
    yanking the view down.

    The caller anchors it — normally bottom-centre over the list — because only
    the caller knows what else is competing for that corner.
*/
StyledButton {
    id: control

    /*! The chat list this returns to the bottom of. */
    property ListView list: null

    /*!
        How far from the newest message counts as "away", in pixels.

        Roughly three message rows. Small enough that a message never arrives
        unseen behind the fold, large enough that nudging the wheel one notch
        does not flash a button at you.
    */
    property int threshold: 96

    /*! True while the list is scrolled past \l threshold from the newest message. */
    readonly property bool awayFromLatest: control.list !== null
        && control.list.contentHeight > control.list.height
        && control.list.contentY < control.list.contentHeight - control.list.height - control.threshold

    text: qsTr("Jump to current")
    primary: true
    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/arrow-down.svg"

    // Driven by opacity so the button fades rather than blinking, and folded
    // back into `visible` so a fully faded button cannot be clicked or take
    // focus.
    opacity: control.awayFromLatest ? 1 : 0
    visible: opacity > 0
    Behavior on opacity {
        NumberAnimation {
            duration: 120
            easing.type: Easing.OutQuad
        }
    }

    onClicked: {
        if (control.list)
            control.list.positionViewAtEnd()
    }
}
