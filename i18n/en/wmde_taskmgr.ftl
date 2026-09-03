app-title = Task Manager

processes = Processes
performance = Performance
users = Users

column-name = Name
column-cpu = CPU
column-memory = Memory
column-data = Data
column-disk = Disk
column-threads = Threads
column-pid = PID

section-applications = Applications
section-background = Background processes
section-system = System processes

group-panel = Panel
group-desktop-shell = Desktop shell


end-task = End task
kill-task = Force quit

size-b = { $value } B
size-kib = { $value } KiB
size-mib = { $value } MiB
size-gib = { $value } GiB

rate-b = { $value } B/s
rate-kib = { $value } KiB/s
rate-mib = { $value } MiB/s
rate-gib = { $value } GiB/s

no-value = -

device-cpu = CPU
device-memory = Memory
device-disk = Disk
device-network = Network

last-seconds = last { $count } seconds
show-per-core = Per logical processor
show-overall = Overall

summary-memory = { $used } of { $total }
summary-disk-read = read { $value }
summary-disk-write = write { $value }
summary-net-rx = in { $value }
summary-net-tx = out { $value }

search = Search

scope-mine = My processes
scope-users = All users
scope-all = Including kernel

fact-utilization = Utilization
fact-speed = Speed
fact-processes = Processes
fact-threads = Threads
fact-handles = Handles
fact-uptime = Up time
fact-base-speed = Base speed:
fact-cpus = Logical processors:
fact-sockets = Sockets:
fact-virtualization = Virtualization:
fact-virtual = Virtual machine:
fact-cache = { $level } cache:
fact-cpufreq-driver = Cpufreq driver:
fact-cpufreq-governor = Cpufreq governor:
fact-yes = Yes
fact-no = No

# Memory page, under the plot. The left half is what the memory is doing, the right half is
# what the modules are - the second only appears when the collector left a report.
fact-mem-in-use = In use
fact-mem-available = Available
fact-mem-committed = Committed
fact-mem-cached = Cached
fact-mem-swap = Swap
fact-mem-committed-value = { $used } / { $limit }
fact-mem-speed = Speed:
fact-mem-speed-value = { $value } MT/s
# The module runs slower than it is rated for, which is the part worth naming.
fact-mem-speed-rated = { $now } MT/s (rated { $max })
fact-mem-slots = Slots used:
fact-mem-slots-value = { $used } of { $total }
fact-mem-kind = Type:
fact-mem-form-factor = Form factor:
fact-mem-channels = Channels:
fact-mem-modules = Modules

speed-ghz = { $value } GHz
