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
column-session = Session

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

# Which login this is. A row on the users page is one login, not one account.
session-desktop = Desktop
session-console = Console { $tty }
session-console-plain = Console
session-remote = ssh { $host }
session-remote-service = { $service } { $host }
session-remote-plain = ssh
# What runs outside every login: portals, pipewire, systemd --user.
session-background = Background services
# Two identical logins of one account are told apart by a node: pts/2 or the session number.
session-node = { $what } ({ $node })

no-value = -

device-cpu = CPU
device-memory = Memory
device-net-ethernet = Ethernet
device-net-wireless = Wi-Fi
device-disk-nvme = NVMe
device-disk-ssd = SSD
device-disk-hdd = HDD
device-disk-removable = Removable
device-gpu = GPU
# What a device is, and under which name the kernel knows it.
device-node = { $what } ({ $node })
device-disk-node = { $node }, { $size }
link-mbit = { $value } Mbit/s
link-gbit = { $value } Gbit/s

last-seconds = last { $count } seconds
show-per-core = Per logical processor
show-overall = Overall

summary-memory = { $used } of { $total }
# Both rates on one line, the way the reference writes "S: 0 R: 0 Kbps".
summary-disk-rate = R: { $read }  W: { $write }
summary-net-rate = In: { $rx }  Out: { $tx }

search = Search

scope-mine = My processes
scope-users = All users
scope-all = Including kernel
# The second part of the scope list: one entry per row of the users page.
scope-logins = Login
scope-login = { $account } - { $session }

show-processes = Show processes

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

# Graphics card page, in the column the processor page fills with its facts.
fact-gpu-memory = Video memory
fact-gpu-driver = Driver:
fact-gpu-location = Location:

speed-ghz = { $value } GHz
