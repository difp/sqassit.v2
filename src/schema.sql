CREATE TABLE IF NOT EXISTS users (
    telegram_id           INTEGER PRIMARY KEY,
    username              TEXT,
    first_name            TEXT,
    phone                 TEXT NOT NULL DEFAULT '', -- введён вручную или из Telegram-контакта
    avatar_file_id        TEXT,      -- file_id аватарки; сам файл НЕ сохраняем на диск
    is_coach              INTEGER NOT NULL DEFAULT 0,
    notifications_enabled INTEGER NOT NULL DEFAULT 1,
    notify_matches        INTEGER NOT NULL DEFAULT 1, -- лента матчей и события матчей
    notify_trainings      INTEGER NOT NULL DEFAULT 1, -- напоминания и изменения записи
    notify_messages       INTEGER NOT NULL DEFAULT 1, -- личные сообщения от пользователей
    notes                 TEXT NOT NULL DEFAULT '',
    bio                   TEXT NOT NULL DEFAULT '',   -- кратко о тренере
    links                 TEXT NOT NULL DEFAULT '[]', -- ссылки тренера (JSON-массив)
    about                 TEXT NOT NULL DEFAULT '',   -- «о себе» игрока (видит тренер)
    display_mode          TEXT NOT NULL DEFAULT 'name', -- name | animal
    show_username         INTEGER NOT NULL DEFAULT 0, -- согласие показывать @username
    onboarding_seen       INTEGER NOT NULL DEFAULT 0, -- памятка при первом запуске
    created_at            INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS trainings (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    title                 TEXT NOT NULL,
    training_type         TEXT NOT NULL DEFAULT 'Тренировка',
    start_at              INTEGER NOT NULL,           -- Unix-время (UTC)
    duration_min          INTEGER NOT NULL DEFAULT 60,
    capacity              INTEGER NOT NULL DEFAULT 4,
    level                 TEXT NOT NULL DEFAULT '',   -- уровень игры
    reminder_enabled      INTEGER NOT NULL DEFAULT 1,
    reminder_offset_min   INTEGER NOT NULL DEFAULT 270,
    reminder_sent         INTEGER NOT NULL DEFAULT 0,
    deleted_at            INTEGER,                    -- мягкое удаление (корзина)
    deleted_by            INTEGER,
    created_by            INTEGER,
    room_code             TEXT,                       -- код комнаты турнира (squashtabs)
    created_at            INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS bookings (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    training_id  INTEGER NOT NULL,
    user_id      INTEGER NOT NULL,
    status       TEXT NOT NULL DEFAULT 'booked',   -- booked | waitlist | cancelled
    position     INTEGER NOT NULL DEFAULT 0,       -- порядок в очереди ожидания (FIFO)
    created_at   INTEGER NOT NULL,
    UNIQUE (training_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_trainings_start ON trainings(start_at);
CREATE INDEX IF NOT EXISTS idx_bookings_training ON bookings(training_id, status, position);

CREATE TABLE IF NOT EXISTS coaches (
    telegram_id INTEGER PRIMARY KEY,
    added_by    INTEGER,                 -- кто выдал доступ (telegram_id), NULL для владельца
    is_manager  INTEGER NOT NULL DEFAULT 0,  -- право подтверждать заявки и вести реестр
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS coach_applications (
    telegram_id INTEGER PRIMARY KEY,
    message     TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'pending',  -- pending | approved | rejected
    created_at  INTEGER NOT NULL,
    decided_by  INTEGER,
    decided_at  INTEGER
);

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    from_user   INTEGER NOT NULL,
    to_user     INTEGER NOT NULL,
    text        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS feedback (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     INTEGER NOT NULL,
    kind        TEXT NOT NULL,             -- bug | idea
    text        TEXT NOT NULL,
    page        TEXT NOT NULL DEFAULT '',  -- откуда отправлено
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS matches (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    title         TEXT NOT NULL DEFAULT 'Матч',
    description   TEXT NOT NULL DEFAULT '',
    location      TEXT NOT NULL DEFAULT '',
    start_at      INTEGER NOT NULL,
    duration_min  INTEGER NOT NULL DEFAULT 60,
    max_players   INTEGER NOT NULL DEFAULT 4,
    level         TEXT NOT NULL DEFAULT '',
    created_by    INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    room_code     TEXT,                       -- код комнаты турнира (squashtabs)
    deleted_at    INTEGER
);

CREATE TABLE IF NOT EXISTS match_players (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    match_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'booked', -- pending | booked | waitlist | rejected
    decided_by  INTEGER,
    decided_at  INTEGER,
    created_at  INTEGER NOT NULL,
    UNIQUE (match_id, user_id)
);

CREATE TABLE IF NOT EXISTS reminder_deliveries (
    training_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    PRIMARY KEY (training_id, user_id)
);

CREATE TABLE IF NOT EXISTS event_notifications (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,             -- training | training_booked | ...
    entity_id   INTEGER NOT NULL,
    chat_id     INTEGER NOT NULL,
    user_id     INTEGER NOT NULL DEFAULT 0, -- 0 = сводка организатору
    message_id  INTEGER NOT NULL DEFAULT 0,
    text        TEXT NOT NULL DEFAULT '',
    updated_at  INTEGER NOT NULL,
    delete_at   INTEGER,                    -- когда удалить сообщение
    phase       TEXT NOT NULL DEFAULT '',   -- preliminary | final | status
    sent_at     INTEGER NOT NULL DEFAULT 0, -- время последней отправки
    final_at    INTEGER,                    -- когда заменить предварительное
    UNIQUE (kind, entity_id, chat_id, user_id)
);

CREATE TABLE IF NOT EXISTS event_coauthors (
    event_type  TEXT NOT NULL,
    event_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    added_by    INTEGER,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (event_type, event_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_matches_start ON matches(start_at);
CREATE INDEX IF NOT EXISTS idx_match_players_match ON match_players(match_id);

CREATE INDEX IF NOT EXISTS idx_messages_to ON messages(to_user, created_at);
