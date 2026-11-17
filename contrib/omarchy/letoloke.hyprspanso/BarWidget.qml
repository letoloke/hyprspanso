import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// hyprspanso status in the bar.
//   accent icon  - daemon running and expanding
//   dimmed icon  - daemon running but paused, or not running at all
// Left click opens the snippet search, right click pauses/resumes, middle
// click reloads the match files.
BarWidget {
  id: root
  moduleName: "letoloke.hyprspanso"

  property bool running: false
  property bool expanding: true
  property bool searching: false
  property int matchCount: 0

  readonly property color baseColor: bar ? bar.barForeground : Commons.Color.foreground

  // Solid when expanding, faded when paused, barely there when not running.
  readonly property color iconColor: {
    if (!running) return Qt.rgba(baseColor.r, baseColor.g, baseColor.b, 0.30)
    if (!expanding) return Qt.rgba(baseColor.r, baseColor.g, baseColor.b, 0.50)
    return baseColor
  }

  // The binary may be in ~/.local/bin (install.sh) or /usr/bin (package).
  readonly property string statusCommand:
    "if command -v hyprspanso >/dev/null 2>&1; then exec hyprspanso status; " +
    "elif [ -x \"$HOME/.local/bin/hyprspanso\" ]; then exec \"$HOME/.local/bin/hyprspanso\" status; " +
    "else exit 127; fi"

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  function refresh() {
    if (!statusProc.running) statusProc.running = true
  }

  function runCommand(action) {
    if (!bar) return
    bar.run("sh -c 'if command -v hyprspanso >/dev/null 2>&1; then hyprspanso " + action +
            "; else \"$HOME/.local/bin/hyprspanso\" " + action + "; fi'")
    refreshSoon.restart()
  }

  readonly property string stateText: {
    if (!running) return "hyprspanso · not running"
    if (!expanding) return "hyprspanso · paused · " + matchCount + " matches"
    return "hyprspanso · active · " + matchCount + " matches" + (searching ? " · searching" : "")
  }

  Process {
    id: statusProc
    command: ["sh", "-c", root.statusCommand]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var m = text.match(/enabled=(true|false)\s+matches=(\d+)\s+searching=(true|false)/)
        if (!m) return
        root.running = true
        root.expanding = m[1] === "true"
        root.matchCount = Number(m[2])
        root.searching = m[3] === "true"
      }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) {
        root.running = false
        root.searching = false
      }
    }
  }

  Timer {
    interval: 3000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  Timer {
    id: refreshSoon
    interval: 400
    repeat: false
    onTriggered: root.refresh()
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: "\uf11c"
    foreground: root.iconColor
    tooltipText: root.stateText
    onPressed: function(mouseButton) {
      if (mouseButton === Qt.LeftButton) root.runCommand("launcher")
      else if (mouseButton === Qt.RightButton) root.runCommand("toggle")
      else if (mouseButton === Qt.MiddleButton) root.runCommand("reload")
    }
  }
}
