-- Builds that ran longer than the sweep's grace margin.
--
-- A window selects builds by completion, so the listing reaches back
-- `CREATE_GRACE_SECS` (eight days) before the window starts to catch
-- builds created earlier. Anything slower than that is missed at the
-- leading edge of a sweep, silently — a build that was never listed leaves
-- nothing behind to notice.
--
-- Run this occasionally against a growing store: if the longest build
-- approaches eight days, the margin needs raising again. As of 2026-08-18,
-- thirteen builds of 1,220,010 exceeded three days and the longest took
-- 6.76 (python-dask, task 146570209).
--
--   sqlite3 lag.sqlite < long-builds.sql
SELECT round((completion_ts - create_ts) / 86400.0, 2) AS days,
       CASE state WHEN 0 THEN 'FREE' WHEN 1 THEN 'OPEN' WHEN 2 THEN 'CLOSED'
                  WHEN 3 THEN 'CANCELED' WHEN 4 THEN 'ASSIGNED'
                  WHEN 5 THEN 'FAILED' ELSE state END AS state,
       coalesce(package, '(unnamed)') AS package,
       date(create_ts, 'unixepoch') AS created,
       date(completion_ts, 'unixepoch') AS finished,
       'https://koji.fedoraproject.org/koji/taskinfo?taskID=' || task_id AS url
FROM builds
WHERE instance = 'fedora'
  AND completion_ts - create_ts > 3 * 86400
ORDER BY days DESC;
