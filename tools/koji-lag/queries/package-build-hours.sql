-- Which packages consume an architecture's build capacity.
--
-- Not all builds are equal: on 2026-07-17, `gcc` alone took 48.9 hours of
-- s390x build time from a pool with sixteen active hosts, while the median
-- build took minutes. A rebuild day's work can exceed the pool's whole
-- daily capacity on the strength of a few packages.
--
--   sqlite3 lag.sqlite < package-build-hours.sql
--
-- Needs package names on the builds; `sync` fills them in from the
-- children, so a store collected before that ran should be re-synced over
-- the same window first (it fetches nothing).
WITH bounds AS (
    SELECT strftime('%s', '2026-07-17') AS lo,
           strftime('%s', '2026-07-18') AS hi
)
SELECT coalesce(b.package, t.package, '(unnamed)') AS package,
       count(*) AS tasks,
       round(sum(t.completion_ts - t.start_ts) / 3600.0, 1) AS build_hours,
       round(avg(t.completion_ts - t.start_ts) / 60.0, 1) AS mean_minutes,
       round(max(t.start_ts - t.create_ts) / 60.0, 1) AS worst_wait_min
FROM tasks t
JOIN builds b ON b.instance = t.instance AND b.task_id = t.parent
CROSS JOIN bounds
WHERE t.instance = 'fedora'
  AND t.arch = 's390x'
  AND t.method = 'buildArch'
  AND t.start_ts IS NOT NULL AND t.completion_ts IS NOT NULL
  AND b.create_ts >= bounds.lo AND b.create_ts < bounds.hi
GROUP BY 1
ORDER BY build_hours DESC
LIMIT 20;
