cmake_minimum_required(VERSION 3.21)
project(conquerd_portal_tests LANGUAGES CXX)
set(CMAKE_AUTOMOC ON)
set(CMAKE_CXX_STANDARD 17)
find_package(Qt6 6.8 REQUIRED COMPONENTS QuickTest WebEngineQuick)

add_executable(portal_tests main.cpp ../../src/ui/scheme.cpp)
target_link_libraries(portal_tests PRIVATE Qt6::QuickTest Qt6::WebEngineQuick)
qt_add_resources(portal_tests portal_icons
    PREFIX "/qt/qml/ConquerD/Client"
    BASE "../../qml"
    FILES "../../qml/icons/warning.svg")

# Exercise the production QML without building the Rust/Qt application.
set(module_dir "${CMAKE_CURRENT_BINARY_DIR}/imports/ConquerD/Client")
file(MAKE_DIRECTORY "${module_dir}")
configure_file(../qml/qmldir "${module_dir}/qmldir" COPYONLY)
file(APPEND "${module_dir}/qmldir" "\nConquerdWebView 1.0 ConquerdWebView.qml\n")
foreach(component Theme StyledButton StyledTextField ConquerdWebView)
    configure_file("../../qml/${component}.qml" "${module_dir}/${component}.qml" COPYONLY)
endforeach()
enable_testing()
add_test(NAME portal_navigation COMMAND portal_tests -platform offscreen
    -input "${CMAKE_CURRENT_SOURCE_DIR}" -import "${CMAKE_CURRENT_BINARY_DIR}/imports" -o -,txt)
set_tests_properties(portal_navigation PROPERTIES TIMEOUT 90)
