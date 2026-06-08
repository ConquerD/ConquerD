// ComponentGallery.qml — Visual inventory for the ConquerD design system.
// Load via a dev entry point or embed in diagnostics when needed.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

ScrollView {
    id: root
    clip: true
    contentWidth: availableWidth

    Rectangle {
        width: root.availableWidth
        implicitHeight: galleryColumn.implicitHeight + Theme.spacingXl * 2
        color: Theme.bg1

        ColumnLayout {
            id: galleryColumn
            x: Theme.spacingXl
            y: Theme.spacingXl
            width: Math.max(0, root.availableWidth - Theme.spacingXl * 2)
            spacing: Theme.spacingXl

            Label {
                text: "ConquerD Component Gallery"
                color: Theme.text
                font.pixelSize: Theme.fontSizeTitle
                font.bold: true
            }

            Label {
                Layout.fillWidth: true
                text: "Token-driven components from DESIGN.md. Toggle Theme.isDark in General settings to preview light mode."
                color: Theme.muted
                font.pixelSize: Theme.fontSizeBody
                wrapMode: Text.WordWrap
            }

            SettingsSectionHeader { title: "Buttons" }

            SettingsCard {
                title: "StyledButton"
                subtitle: "Primary, neutral, success, and danger variants."

                RowLayout {
                    spacing: Theme.spacingSm
                    StyledButton { text: "Primary"; primary: true }
                    StyledButton { text: "Neutral" }
                    StyledButton { text: "Success"; success: true }
                    StyledButton { text: "Danger"; danger: true }
                }

                RowLayout {
                    spacing: Theme.spacingSm
                    StyledButton {
                        text: "With Icon"
                        primary: true
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/invite.svg"
                    }
                    StyledButton {
                        text: "Compact"
                        compact: true
                    }
                }
            }

            SettingsSectionHeader { title: "Inputs" }

            SettingsCard {
                title: "StyledTextField"
                subtitle: "Accent focus ring on bg3 surface."

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: Theme.spacingSm

                    StyledTextField {
                        Layout.fillWidth: true
                        placeholderText: "Placeholder text"
                    }
                    StyledTextField {
                        Layout.fillWidth: true
                        text: "Focused state — click to test"
                    }
                }
            }

            SettingsSectionHeader { title: "Toggles" }

            SettingsCard {
                SettingSwitch {
                    title: "Example switch"
                    description: "Uses Theme spacing, typography, and state colours."
                    checked: true
                }
                SettingSwitch {
                    title: "Disabled switch"
                    description: "50% opacity when disabled."
                    checked: false
                    enabledState: false
                }
            }

            SettingsSectionHeader { title: "Navigation" }

            SettingsCard {
                title: "SidebarItem"
                subtitle: "Selected state: accent fill + 3px left bar."

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0

                    SidebarItem {
                        width: parent.width
                        iconSource: "qrc:/qt/qml/ConquerD/Client/icons/speech.svg"
                        label: "Chat"
                        badge: 3
                        selected: true
                    }
                    SidebarItem {
                        width: parent.width
                        iconSource: "qrc:/qt/qml/ConquerD/Client/icons/gear.svg"
                        label: "Settings"
                        badge: 0
                        selected: false
                    }
                }
            }

            SettingsSectionHeader { title: "Status" }

            SettingsCard {
                title: "SessionBanner states"
                subtitle: "Semantic connection-mode colours."

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: Theme.spacingXs

                    SessionBanner {
                        Layout.fillWidth: true
                        height: Theme.bannerHeight
                        connectionMode: "direct"
                        bannerText: "Connected peer-to-peer"
                    }
                    SessionBanner {
                        Layout.fillWidth: true
                        height: Theme.bannerHeight
                        connectionMode: "relay"
                        bannerText: "Relayed via supernode"
                    }
                    SessionBanner {
                        Layout.fillWidth: true
                        height: Theme.bannerHeight
                        connectionMode: "offline"
                        bannerText: "No active session"
                    }
                }
            }

            SettingsSectionHeader { title: "Empty States" }

            SettingsCard {
                title: "EmptyState"
                subtitle: "Shared placeholder for lists with no content."

                EmptyState {
                    Layout.fillWidth: true
                    iconSource: "qrc:/qt/qml/ConquerD/Client/icons/peers.svg"
                    iconSize: 32
                    title: "No peers yet"
                    subtitle: "Paste an invite above to add a trusted peer."
                }
            }

            SettingsSectionHeader { title: "Palette" }

            SettingsCard {
                title: "Background stack"
                subtitle: "bg0 → bg1 → bg2 → bg3"

                GridLayout {
                    columns: 4
                    columnSpacing: Theme.spacingSm
                    rowSpacing: Theme.spacingSm

                    Repeater {
                        model: [
                            { name: "bg0", color: Theme.bg0 },
                            { name: "bg1", color: Theme.bg1 },
                            { name: "bg2", color: Theme.bg2 },
                            { name: "bg3", color: Theme.bg3 },
                            { name: "accent", color: Theme.accent },
                            { name: "online", color: Theme.online },
                            { name: "warn", color: Theme.warn },
                            { name: "danger", color: Theme.danger }
                        ]

                        delegate: ColumnLayout {
                            spacing: Theme.spacingXs

                            Rectangle {
                                Layout.preferredWidth: 72
                                Layout.preferredHeight: 40
                                color: modelData.color
                                border.color: Theme.divider
                                border.width: 1
                            }

                            Label {
                                text: modelData.name
                                color: Theme.muted
                                font.pixelSize: Theme.fontSizeCaption
                            }
                        }
                    }
                }
            }
        }
    }
}