import QtQuick
import QtQuick.Controls.Material
import QtTest
import ConquerD.Client 1.0

Item {
    width: 640
    height: 360

    StyledButton {
        id: button
        x: 20
        y: 20
        width: 120
        text: "A deliberately long action label"
    }

    StyledTextField {
        id: field
        x: 20
        y: 80
        width: 240
        placeholderText: "Peer name"
    }

    TestCase {
        name: "SharedControls"
        when: windowShown

        SignalSpy {
            id: clickSpy
            target: button
            signalName: "clicked"
        }

        function cleanup() {
            mouseRelease(button, button.width / 2, button.height / 2)
            button.enabled = true
            button.primary = false
            button.text = "A deliberately long action label"
            button.icon.source = ""
            field.enabled = true
            Theme.isDark = true
            clickSpy.clear()
            field.forceActiveFocus(Qt.OtherFocusReason)
        }

        function luminance(color) {
            var channels = [color.r, color.g, color.b].map(function(value) {
                return value <= 0.04045 ? value / 12.92 : Math.pow((value + 0.055) / 1.055, 2.4)
            })
            return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722
        }

        function test_secondaryTextContrast() {
            for (var mode = 0; mode < 2; mode++) {
                Theme.isDark = mode === 0
                var foreground = luminance(Theme.muted)
                var surfaces = [Theme.bg0, Theme.bg1, Theme.bg2, Theme.bg3]
                for (var surface = 0; surface < surfaces.length; surface++) {
                    var background = luminance(surfaces[surface])
                    var ratio = (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05)
                    verify(ratio >= 4.5, "Secondary text contrast: " + ratio)
                }
            }
        }

        function test_iconLayout() {
            button.icon.source = Qt.resolvedUrl("../../qml/icons/invite.svg")
            var row = button.contentItem.children[0]
            var image = row.children[0]
            var label = row.children[1]
            tryCompare(image, "status", Image.Ready)
            tryVerify(function() { return label.x >= image.x + image.width })
            verify(label.x + label.width <= row.width)
            button.text = ""
            tryVerify(function() { return Math.abs(image.x + image.width / 2 - row.width / 2) < 1 })
        }

        function test_keyboardActivation() {
            button.forceActiveFocus(Qt.TabFocusReason)
            keyClick(Qt.Key_Space)
            compare(clickSpy.count, 1)
            button.enabled = false
            keyClick(Qt.Key_Space)
            compare(clickSpy.count, 1)
        }

        function test_verticalCentering() {
            var row = button.contentItem.children[0]
            var image = row.children[0]
            var label = row.children[1]
            compare(button.verticalPadding, 0)
            compare(button.topPadding, 0)
            compare(button.bottomPadding, 0)
            compare(button.height, Theme.controlHeight)
            button.text = "Invite"
            button.icon.source = Qt.resolvedUrl("../../qml/icons/invite.svg")
            tryCompare(image, "status", Image.Ready)
            tryVerify(function() {
                return Math.abs(label.mapToItem(button, 0, label.height / 2).y - button.height / 2) <= 1
            }, 1000, "Invite label must be vertically centered")
            verify(Math.abs(image.mapToItem(button, 0, image.height / 2).y - button.height / 2) <= 1,
                   "Invite icon must be vertically centered")
            button.text = "Accept invite"
            button.icon.source = ""
            tryVerify(function() {
                return Math.abs(label.mapToItem(button, 0, label.height / 2).y - button.height / 2) <= 1
            }, 1000, "Text-only label must be vertically centered")
            // Even if a layout stretches the control to title-bar height,
            // icon and label stay in the vertical center — not the bottom.
            button.height = Theme.titleBarHeight
            button.text = "Invite"
            button.icon.source = Qt.resolvedUrl("../../qml/icons/invite.svg")
            tryCompare(image, "status", Image.Ready)
            tryVerify(function() {
                return Math.abs(label.mapToItem(button, 0, label.height / 2).y - button.height / 2) <= 1
            }, 1000, "Invite label must stay centered in a title-bar-tall button")
            verify(Math.abs(image.mapToItem(button, 0, image.height / 2).y - button.height / 2) <= 1,
                   "Invite icon must stay centered in a title-bar-tall button")
            button.height = Theme.controlHeight
        }

        function test_buttonLabelFits() {
            var row = button.contentItem.children[0]
            var label = row.children[1]
            compare(row.width, button.availableWidth)
            verify(label.truncated)
            verify(label.x + label.width <= row.width)
            compare(button.height, Theme.controlHeight)
            compare(button.background.height, button.height)
        }

        function test_neutralPressedState() {
            mousePress(button, button.width / 2, button.height / 2)
            verify(button.down)
            tryCompare(button.background, "color", Theme.selectedFill())
        }

        function test_keyboardFocus() {
            button.forceActiveFocus(Qt.TabFocusReason)
            verify(button.visualFocus)
            compare(button.background.border.width, 2)
            tryCompare(button.background.border, "color", Theme.text)
            keyClick(Qt.Key_Tab)
            verify(field.activeFocus)
            compare(field.background.border.width, 2)
        }

        function test_fieldThemes() {
            for (var mode = 0; mode < 2; mode++) {
                Theme.isDark = mode === 0
                compare(field.background.color, Theme.bg2)
                compare(field.placeholderTextColor, Theme.muted)
                compare(field.selectionColor, Theme.accent)
                field.enabled = false
                compare(field.background.color, Theme.bg1)
                compare(field.color, Theme.muted)
                field.enabled = true
            }
        }
    }
}