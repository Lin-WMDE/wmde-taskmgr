app-title = Диспетчер завдань

processes = Процеси
performance = Продуктивність
users = Користувачі

column-name = Імʼя
column-cpu = ЦП
column-memory = Памʼять
column-data = Дані
column-disk = Диск
column-gpu = ГП
column-gpu-memory = Памʼять ГП
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
device-gpu = Відеокарта
device-node = { $what } ({ $node })
device-disk = Диск
device-disk-aside = { $model }, { $size }
link-mbit = { $value } Мбіт/с
link-gbit = { $value } Гбіт/с

last-seconds = останні { $count } секунд
show-per-core = За логічними процесорами
show-overall = Загалом

summary-memory = { $used } з { $total }
summary-disk-rate = Ч: { $read }  З: { $write }
summary-net-rate = Пр: { $rx }  Пер: { $tx }
summary-temperature = { $reading } ({ $temp } °C)
temperature-c = { $value } °C

search = Пошук

scope-mine = Мої процеси
scope-users = Усі користувацькі
scope-all = Разом із ядерними
# Друга частина списку охоплення: по пункту на кожен рядок вкладки користувачів.
scope-logins = Вхід
scope-login = { $account } - { $session }

show-processes = Показати процеси

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

fact-gpu-memory = Відеопамʼять
fact-gpu-memory-busy = Контролер памʼяті
fact-gpu-shared = Спільна памʼять
fact-temperature = Температура
fact-gpu-core-clock = Частота ядра:
fact-gpu-memory-clock = Частота памʼяті:
fact-gpu-power = Споживання:
fact-gpu-fan = Вентилятор:
clock-mhz = { $value } МГц
power-w = { $value } Вт
fan-rpm = { $value } об/хв
engine-3d = 3D
engine-compute = Обчислення
engine-copy = Копіювання
engine-video = Відео
engine-video-enhance = Обробка відео
engine-decode = Декодування відео
engine-encode = Кодування відео
engine-encode-2 = Кодування відео 2
engine-jpeg = JPEG
fact-gpu-chip = Чіп:
fact-gpu-driver = Драйвер:
fact-gpu-node = Вузол:
fact-gpu-location = Розташування:

net-receive = Отримання
net-send = Надсилання
net-received = Отримано
net-sent = Надіслано
net-signal = Сигнал
net-pair = { $rx } / { $tx }
net-band = { $value } ГГц
net-duplex-full = повний
net-duplex-half = половинний
fact-net-state = Стан:
fact-net-disconnected = Відключено
fact-net-type = Тип підключення:
fact-net-ssid = SSID:
fact-net-band = Діапазон:
fact-net-speed = Швидкість:
fact-net-duplex = Дуплекс:
fact-net-mac = MAC-адреса:
fact-net-mtu = MTU:
fact-net-driver = Драйвер:
fact-net-interface = Інтерфейс:
fact-net-packets = Пакети:
fact-net-errors = Помилки:
fact-net-dropped = Відкинуто:
fact-net-ipv4 = IPv4-адреса
fact-net-ipv6 = IPv6-адреса
fact-net-gateway = Основний шлюз

disk-read = Читання
disk-write = Запис
fact-disk-active = Активний час
fact-disk-response = Середній час відгуку
response-ms = { $value } мс
fact-disk-type = Тип:
fact-disk-capacity = Ємність:
fact-disk-formatted = Відформатовано:
fact-disk-system = Системний диск:
fact-disk-swap = Файл підкачки:
fact-disk-mounts = Точки монтування

speed-ghz = { $value } ГГц
