app-title = Диспетчер завдань

processes = Процеси
performance = Продуктивність
users = Користувачі

column-name = Імʼя
column-cpu = ЦП
column-memory = Памʼять
column-data = Дані
column-disk = Диск
column-threads = Потоки
column-pid = PID
column-session = Сеанс

section-applications = Програми
section-background = Фонові процеси
section-system = Системні процеси

group-panel = Панель
group-desktop-shell = Оболонка стільниці


end-task = Зняти завдання
kill-task = Завершити примусово

size-b = { $value } Б
size-kib = { $value } КіБ
size-mib = { $value } МіБ
size-gib = { $value } ГіБ

rate-b = { $value } Б/с
rate-kib = { $value } КіБ/с
rate-mib = { $value } МіБ/с
rate-gib = { $value } ГіБ/с

# Який це вхід. Рядок вкладки користувачів - один вхід, а не один обліковий запис.
session-desktop = Робочий стіл
session-console = Консоль { $tty }
session-console-plain = Консоль
session-remote = ssh { $host }
session-remote-service = { $service } { $host }
session-remote-plain = ssh
# Те, що працює поза будь-яким входом: portals, pipewire, systemd --user.
session-background = Фонові служби
# Два однакові входи одного облікового запису розводяться вузлом: pts/2 або номер сеансу.
session-node = { $what } ({ $node })

no-value = -

device-cpu = Процесор
device-memory = Памʼять
device-net-ethernet = Ethernet
device-net-wireless = Wi-Fi
device-disk-nvme = NVMe
device-disk-ssd = SSD
device-disk-hdd = HDD
device-disk-removable = Змінний
device-node = { $what } ({ $node })
device-disk-node = { $node }, { $size }
link-mbit = { $value } Мбіт/с
link-gbit = { $value } Гбіт/с

last-seconds = останні { $count } секунд
show-per-core = За логічними процесорами
show-overall = Загалом

summary-memory = { $used } з { $total }
summary-disk-rate = Ч: { $read }  З: { $write }
summary-net-rate = Пр: { $rx }  Пер: { $tx }

search = Пошук

scope-mine = Мої процеси
scope-users = Усі користувацькі
scope-all = Разом із ядерними

fact-utilization = Завантаження
fact-speed = Швидкість
fact-processes = Процесів
fact-threads = Потоків
fact-handles = Дескрипторів
fact-uptime = Час роботи
fact-base-speed = Базова швидкість:
fact-cpus = Логічних процесорів:
fact-sockets = Сокетів:
fact-virtualization = Віртуалізація:
fact-virtual = Віртуальна машина:
fact-cache = Кеш { $level }:
fact-cpufreq-driver = Драйвер cpufreq:
fact-cpufreq-governor = Регулятор cpufreq:
fact-yes = Так
fact-no = Ні

fact-mem-in-use = Зайнято
fact-mem-available = Доступно
fact-mem-committed = Виділено
fact-mem-cached = Кешовано
fact-mem-swap = Підкачка
fact-mem-committed-value = { $used } / { $limit }
fact-mem-speed = Швидкість:
fact-mem-speed-value = { $value } МТ/с
fact-mem-speed-rated = { $now } МТ/с (номінал { $max })
fact-mem-slots = Слотів зайнято:
fact-mem-slots-value = { $used } з { $total }
fact-mem-kind = Тип:
fact-mem-form-factor = Форм-фактор:
fact-mem-channels = Каналів:
fact-mem-modules = Модулі

speed-ghz = { $value } ГГц
