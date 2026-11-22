package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class IpcTest {
    private val ws1 = IpcWorkspaceRef(1, "1")
    private val ws2 = IpcWorkspaceRef(2, "web")

    private fun win(id: Int, ws: IpcWorkspaceRef = ws1, title: String = "t$id", floating: Boolean = false) = IpcWindow(
        taskId = id, cls = "com.example.app$id", title = title, x = 10, y = 48, width = 800, height = 600,
        workspace = ws, floating = floating, pinned = false, fullscreen = 0, hidden = false,
        monitor = 0, pid = 1000 + id, focusHistoryId = 0,
    )

    private fun snap(
        windows: List<IpcWindow>,
        active: IpcWorkspaceRef = ws1,
        workspaces: List<IpcWorkspaceRef> = listOf(ws1),
        focused: Int = -1,
        special: IpcWorkspaceRef? = null,
        urgent: Set<Int> = emptySet(),
    ) = IpcSnapshot("DISPLAY-0", active, workspaces, special, windows, focused, urgent)

    @Test
    fun parsesFlagsAndArguments() {
        assertEquals(IpcRequest("j", "clients", ""), IpcRequest.parse("j/clients"))
        assertEquals(IpcRequest("", "dispatch", "exec /system/bin/sh -c x"),
            IpcRequest.parse("dispatch exec /system/bin/sh -c x"))
        assertEquals(IpcRequest("", "dispatch", "workspace 2"), IpcRequest.parse("/dispatch workspace 2"))
        assertEquals(IpcRequest("jr", "getoption", "general:gaps_in"), IpcRequest.parse("jr/getoption general:gaps_in"))
        assertTrue(IpcRequest.parse("j/monitors").json)
        assertEquals(listOf("workspace", "2"), IpcRequest.parse("dispatch workspace  2").argv)
    }

    @Test
    fun splitsBatchesWithEscapes() {
        assertEquals(listOf("j/clients", "dispatch workspace 2"),
            IpcRequest.splitBatch("[[BATCH]]j/clients ; dispatch workspace 2;"))
        assertEquals(listOf("eval a = 1; b = 2", "x\\y"),
            IpcRequest.splitBatch("[[BATCH]]eval a = 1\\; b = 2;x\\\\y"))
    }

    @Test
    fun jsonEscapesAndIndents() {
        assertEquals("\"a\\\"b\\n\\u0001\"", Json.quote("a\"b\n\u0001"))
        assertEquals("{\n    \"a\": [1,2],\n    \"b\": {\n        \"c\": true\n    }\n}",
            Json.write(linkedMapOf("a" to listOf(1, 2), "b" to linkedMapOf("c" to true))))
        assertEquals("[]", Json.write(emptyList<Int>()))
        assertEquals("1.50000", Json.write(1.5f))
    }

    @Test
    fun clientsUseHyprlandFieldNames() {
        val json = HyprCtlFormat.clients(listOf(win(0x2a)), json = true)
        for (field in listOf("\"address\": \"0x2a\"", "\"mapped\": true", "\"at\": [10,48]", "\"size\": [800,600]",
            "\"workspace\": {", "\"id\": 1", "\"class\": \"com.example.app42\"", "\"floating\": false",
            "\"pinned\": false", "\"fullscreen\": 0", "\"focusHistoryID\": 0", "\"pid\": 1042")) {
            assertTrue("$field in $json", json.contains(field))
        }
        val text = HyprCtlFormat.clients(listOf(win(0x2a)), json = false)
        assertTrue(text.startsWith("Window 2a -> t42:\n\tmapped: 1\n\thidden: 0\n\tat: 10,48\n\tsize: 800,600\n\tworkspace: 1 (1)\n"))
        assertTrue(text.endsWith("contentType: none\n\n"))
    }

    @Test
    fun activeWindowWithoutOne() {
        assertEquals("Invalid\n", HyprCtlFormat.activeWindow(null, false))
        assertEquals("{}", HyprCtlFormat.activeWindow(null, true))
    }

    @Test
    fun workspacesAndMonitorsText() {
        val ws = IpcWorkspace(ws2, "DISPLAY-0", 0, 2, false, 0x30, "Files", false, "dwindle")
        assertEquals("workspace ID 2 (web) on monitor DISPLAY-0:\n\tmonitorID: 0\n\twindows: 2\n" +
            "\thasfullscreen: 0\n\tlastwindow: 0x30\n\tlastwindowtitle: Files\n\tispersistent: 0\n" +
            "\ttiledLayout: dwindle\n\n", HyprCtlFormat.workspaceText(ws))
        val m = IpcMonitor(0, "DISPLAY-0", "Built-in Screen", "Android", "Built-in Screen", "", 2538, 1058, 0, 0,
            60f, 0, 0, ws1, IpcWorkspaceRef.NONE, listOf(0, 32, 0, 0), 1.125f, 0, true, true, false, "none",
            listOf("2538x1058@60.00Hz"))
        val text = HyprCtlFormat.monitorText(m)
        assertTrue(text, text.startsWith("Monitor DISPLAY-0 (ID 0):\n\t2538x1058@60.00000 at 0x0\n"))
        assertTrue(text.contains("\treserved: 0 32 0 0\n\tscale: 1.13\n"))
        val json = HyprCtlFormat.monitors(listOf(m), true)
        assertTrue(json.contains("\"activeWorkspace\": {\n        \"id\": 1,"))
        assertTrue(json.contains("\"refreshRate\": 60.00000"))
    }

    @Test
    fun bindsCarryModmask() {
        val b = IpcBind(setOf(Mod.ALT, Mod.SHIFT), "Q", "killactive", "")
        assertEquals(9, b.modmask)
        assertEquals("bind\n\tmodmask: 9\n\tsubmap: \n\tkey: Q\n\tkeycode: 0\n\tcatchall: false\n" +
            "\tdescription: \n\tdispatcher: killactive\n\targ: \n\n", HyprCtlFormat.binds(listOf(b), false))
        assertTrue(HyprCtlFormat.binds(listOf(b.copy(flags = "m")), true).contains("\"mouse\": true"))
    }

    @Test
    fun layersByLevel() {
        val bar = IpcLayer.decode(1, "2 bar 0 0 2538 32 500")!!
        assertEquals(IpcLayer(1, 2, "bar", 0, 0, 2538, 32, 500), bar)
        val text = HyprCtlFormat.layers(mapOf("DISPLAY-0" to listOf(bar)), false)
        assertTrue(text.contains("\tLayer level 2 (top):\n\t\tLayer 1: xywh: 0 0 2538 32, namespace: bar, pid: 500\n"))
        assertTrue(HyprCtlFormat.layers(mapOf("DISPLAY-0" to listOf(bar)), true).contains("\"namespace\": \"bar\""))
    }

    @Test
    fun optionTextAndJson() {
        assertEquals("int: 5\nset: true\n", HyprCtlFormat.option("general:gaps_in", "int", "5", true, false))
        assertEquals("float: 0.500000\nset: false\n", HyprCtlFormat.option("x", "float", "0.5", false, false))
        assertTrue(HyprCtlFormat.option("general:gaps_in", "int", "5", true, true).contains("\"int\": 5"))
    }

    @Test
    fun noEventsOnTheFirstSnapshot() {
        assertEquals(emptyList<Pair<String, String>>(), IpcEvents.diff(null, snap(listOf(win(1)))))
    }

    @Test
    fun windowLifecycleEvents() {
        val a = snap(emptyList())
        val b = snap(listOf(win(0x10)), focused = 0x10)
        assertEquals(listOf(
            "openwindow" to "10,1,com.example.app16,t16",
            "activewindow" to "com.example.app16,t16",
            "activewindowv2" to "10",
        ), IpcEvents.diff(a, b))
        val c = snap(listOf(win(0x10, ws2, title = "new", floating = true).copy(pinned = true, fullscreen = 2)),
            active = ws2, workspaces = listOf(ws1, ws2), focused = 0x10)
        assertEquals(listOf(
            "createworkspace" to "web",
            "createworkspacev2" to "2,web",
            "workspace" to "web",
            "workspacev2" to "2,web",
            "movewindow" to "10,web",
            "movewindowv2" to "10,2,web",
            "changefloatingmode" to "10,1",
            "pin" to "10,1",
            "fullscreen" to "1",
            "windowtitle" to "10",
            "windowtitlev2" to "10,new",
            "activewindow" to "com.example.app16,new",
        ), IpcEvents.diff(b, c))
        val d = snap(emptyList(), active = ws1, workspaces = listOf(ws1))
        assertEquals(listOf(
            "workspace" to "1",
            "workspacev2" to "1,1",
            "closewindow" to "10",
            "activewindow" to ",",
            "activewindowv2" to "",
            "destroyworkspace" to "web",
            "destroyworkspacev2" to "2,web",
        ), IpcEvents.diff(c, d))
    }

    @Test
    fun specialAndUrgentEvents() {
        val special = IpcWorkspaceRef(-99, "special:term")
        val a = snap(listOf(win(1)))
        val b = snap(listOf(win(1)), special = special, urgent = setOf(1))
        assertEquals(listOf(
            "activespecial" to "special:term,DISPLAY-0",
            "activespecialv2" to "-99,special:term,DISPLAY-0",
            "urgent" to "1",
        ), IpcEvents.diff(a, b))
        assertEquals(listOf(
            "activespecial" to ",DISPLAY-0",
            "activespecialv2" to ",,DISPLAY-0",
        ), IpcEvents.diff(b, a.copy(urgent = setOf(1))))
    }
}
