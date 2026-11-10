// Hermes bar widget for the Omarchy shell.
//
// Reads the client's status snapshot through `hermes-desktop status --json`
// and never imports app internals. The client writes that file; when it is not
// running the snapshot reports `available: false` and the widget stays quiet.
import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui

BarWidget {
  id: root
  moduleName: "io.github.hermes.gpui"

  property var snapshot: ({ available: false, connected: false, busy: false, approvals: 0 })
  property bool known: false

  readonly property string binary: Quickshell.env("HERMES_GPUI_BIN") || "hermes-desktop"
  readonly property int pollMs: Math.max(2, Number(setting("refreshIntervalSec", 5))) * 1000

  readonly property bool connected: snapshot.connected === true
  readonly property bool busy: snapshot.busy === true
  readonly property int approvals: Number(snapshot.approvals || 0)
  readonly property bool alarming: approvals > 0 || (known && !connected)

  readonly property string barText: {
    if (!root.known || !snapshot.available) return "H"
    if (root.approvals > 0) return "H ⚠" + root.approvals
    if (root.busy) return "H …"
    return root.connected ? "H ●" : "H ○"
  }

  readonly property string tooltipText: {
    if (!root.known || !snapshot.available) return "Hermes: not running\nClick: launch"
    var lines = []
    lines.push("Hermes  " + (root.connected ? "connected" : "offline")
      + "  ·  profile " + String(snapshot.profile || "default"))
    if (root.busy) lines.push("Streaming a reply")
    if (root.approvals > 0) lines.push("⚠ " + root.approvals + " pending approval" + (root.approvals === 1 ? "" : "s"))
    if (snapshot.lastReply) lines.push("\n" + String(snapshot.lastReply))
    lines.push("\nClick: show or hide  ·  Middle click: refresh")
    return lines.join("\n")
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  function refresh() {
    if (!statusProc.running) statusProc.running = true
  }

  function toggleWindow() {
    if (!toggleProc.running) toggleProc.running = true
  }

  Process {
    id: statusProc
    command: [root.binary, "status", "--json"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        try {
          root.snapshot = JSON.parse(text)
          root.known = true
        } catch (e) {
          root.snapshot = ({ available: false, connected: false, busy: false, approvals: 0 })
          root.known = false
        }
      }
    }
  }

  Process {
    id: toggleProc
    command: [root.binary, "--toggle"]
    onExited: root.refresh()
  }

  Timer {
    interval: root.pollMs
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.barText
    active: root.alarming
    tooltipText: root.tooltipText
    onPressed: function(buttonCode) {
      if (buttonCode === Qt.LeftButton) root.toggleWindow()
      else if (buttonCode === Qt.MiddleButton) root.refresh()
    }
  }
}
