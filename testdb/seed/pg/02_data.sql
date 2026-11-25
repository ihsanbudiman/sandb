-- sandb test seed (PostgreSQL): deterministic data.
INSERT INTO customers (name, email, country, plan, created_at)
SELECT n.name,
       lower(replace(n.name, ' ', '.')) || '.' || i || '@mail.com',
       (ARRAY['ID','US','DE','SG','JP','NL'])[1 + i % 6],
       (ARRAY['free','pro','team'])[1 + i % 3],
       date '2026-01-01' + (i % 300)
FROM generate_series(1, 500) AS i
CROSS JOIN LATERAL (SELECT (ARRAY['Livia Saris','Jaydon Aminoff','Maria Lubin','Ann Press','John Clarkson',
  'Aspen Lubin','Kierra Gouse','Omar Dias','Mila Novak','Theo Grant'])[1 + i % 10] AS name) n;

INSERT INTO products (sku, name, price, stock)
SELECT 'SKU-' || (1000 + i),
       (ARRAY['Desk','Lamp','Chair','Monitor','Keyboard'])[1 + i % 5] || ' ' || i,
       5 + (i * 37 % 40000) / 100.0,
       CASE WHEN i % 7 = 0 THEN NULL ELSE i * 13 % 300 END
FROM generate_series(1, 500) AS i;

INSERT INTO orders (customer_id, status, total, paid, shipped_at)
SELECT 1 + (i * 7) % 500,
       (ARRAY['paid','shipped','pending','cancelled','refunded'])[1 + i % 5],
       5 + (i * 7919 % 90000) / 100.0,
       i % 10 >= 3,
       CASE WHEN i % 5 = 1 THEN date '2026-10-01' + (i % 30) ELSE NULL END
FROM generate_series(1, 500) AS i;

INSERT INTO order_items (order_id, product_id, qty, price)
SELECT 1 + (i * 3) % 500, 1 + (i * 11) % 500, 1 + i % 5, 5 + (i * 211 % 20000) / 100.0
FROM generate_series(1, 500) AS i;

INSERT INTO notes (title, body, pinned, created_at)
SELECT (ARRAY['Weekly sync','Ideas','Groceries','Reading list','Bug notes','Trip plan','Draft post'])[1 + i % 7] || ' #' || i,
       CASE WHEN i % 9 = 0 THEN NULL
            ELSE (ARRAY['Call Ann about Q4','Milk, eggs, coffee','Fix login timeout','Book train to Bandung'])[1 + i % 4] END,
       i % 10 >= 7,
       timestamptz '2026-10-01 09:00:00+00' + (i || ' hours')::interval
FROM generate_series(1, 500) AS i;

INSERT INTO tags (name)
SELECT 'tag-' || i FROM generate_series(1, 20) AS i;

INSERT INTO note_tags (note_id, tag_id)
SELECT 1 + (i * 17) % 500, 1 + i % 20 FROM generate_series(1, 120) AS i;

INSERT INTO order_status_history (order_id, seq, status, changed_at)
SELECT 1 + (i - 1) / 2, 1 + (i - 1) % 2,
       CASE WHEN (i - 1) % 2 = 0 THEN 'pending' ELSE 'paid' END,
       timestamptz '2026-10-01 00:00:00+00' + (i || ' minutes')::interval
FROM generate_series(1, 500) AS i;

INSERT INTO analytics.daily_revenue (day, orders, revenue, refunds)
SELECT date '2026-06-01' + (i - 1), 20 + i * 13 % 180, 5 + (i * 104729 % 2500000) / 100.0, (i * 811 % 90000) / 100.0
FROM generate_series(1, 120) AS i;

INSERT INTO analytics.events (user_id, name, source, created_at)
SELECT 1 + i % 500,
       (ARRAY['page_view','signup','login','add_to_cart','checkout'])[1 + i % 5],
       (ARRAY['web','ios','android'])[1 + i % 3],
       timestamptz '2026-10-01 00:00:00+00' + (i || ' minutes')::interval
FROM generate_series(1, 10000) AS i;

INSERT INTO type_zoo (c_int, c_bigint, c_numeric, c_float, c_bool, c_text, c_varchar, c_uuid, c_json, c_jsonb,
  c_date, c_timestamp, c_timestamptz, c_time, c_interval, c_bytea, c_int_arr, c_text_arr, c_enum) VALUES
(1, 9007199254740993, 1234.5678, 3.14159, true, 'plain text', 'varchar', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
 '{"a": 1, "b": [1, 2]}', '{"nested": {"k": "v"}}', '2026-11-02', '2026-11-02 10:30:00', '2026-11-02 10:30:00+07',
 '23:59:59', '1 day 02:03:04', '\xdeadbeef', '{1,2,3}', '{"a","b c"}', 'happy'),
(-2147483648, -9223372036854775808, -0.0001, -1.5e300, false, 'Unicode: café, 日本語, 🚀🎉', 'emoji 👍', '00000000-0000-0000-0000-000000000000',
 '[]', '{}', '1970-01-01', '1999-12-31 23:59:59.999999', '2000-01-01 00:00:00+00', '00:00:00', '-3 months', '\x00', '{}', '{}', 'sad'),
(NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);

-- Fresh statistics, as a real database has: without them the planner can sort a whole table
-- where the primary key gives the order, and the result then depends on when autovacuum runs.
-- shop_staging (a template copy, 03_roles.sql) gets the same statistics.
ANALYZE;
