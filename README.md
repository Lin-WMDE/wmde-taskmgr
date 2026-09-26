# wmde-taskmgr

Task manager for WMDE. Reference: the Windows 11 Task Manager, `ref/taskman/` in the WMDE tree.

Written here rather than forked, so it has no upstream and is never merged from anywhere. COSMIC
1.9 ships a system monitor of its own, cosmic-monitor; WMDE does not take it and keeps this one.

Three tabs behind a navigation rail:

- **Processes** - one row per application, grouped, over a flat list of the processes behind it
- **Performance** - CPU, memory, disk and network over the last minute
- **Users** - the same sample folded by user

Two numbers stand for memory, and the difference between them is the point:

    RSS    resident set; a page shared by fifteen processes is counted fifteen times
    data   RssAnon + max(0, RssFile - VmExe - VmLib), which drops shared program text

`smaps_rollup` is not read at all. It is the only source of PSS and of private memory, and it
costs 1.7 s to walk 1657 processes while refusing two thirds of them without root - so neither
figure can be a live column. The `data` metric comes from `/proc/PID/status`, which is one
read, readable for every process, and a lower bound by construction.

Per-process network traffic is not shown because Linux does not have it: `/proc/PID/net/dev`
holds the counters of the network namespace, not of the process.

Windows are matched to processes over the bus, through `fun.wmde.Comp.Windows` in the
compositor: a Wayland client cannot learn another client's pid on its own.

Handbook: `.doc/taskmgr/` in the WMDE tree.
