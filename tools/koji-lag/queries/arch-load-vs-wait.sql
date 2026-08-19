-- How an architecture's queue wait responds to how much work it is given.
--
-- The question behind it: is a slow day slow because the whole hub is
-- busy, or because that architecture specifically got more than it can
-- take? Measured on Fedora s390x, quiet days serve tasks in about four
-- minutes and the wait climbs with load, breaking sharply above a
-- thousand tasks — while days with far more *total* builds, but little
-- s390x work, stay fast.
--
--   sqlite3 lag.sqlite < arch-load-vs-wait.sql
--
-- Edit the arch below.
--
-- Bucket by the day a task *started*, not the day it was created. A task
-- created on a quiet Thursday that only starts once Friday's mass rebuild
-- drains has waited six days, and charging that wait to Thursday makes the
-- quietest days look like the worst — which is exactly what the first
-- version of this query reported. The wait belongs beside the load that
-- caused it, which is the load when it was served.
WITH per_day AS (
    SELECT date(t.start_ts, 'unixepoch') AS day,
           count(*) AS tasks,
           avg(t.start_ts - t.create_ts) AS mean_wait_s,
           max(t.start_ts - t.create_ts) AS worst_wait_s,
           sum(t.completion_ts - t.start_ts) / 3600.0 AS build_hours
    FROM tasks t
    WHERE t.instance = 'fedora'
      AND t.arch = 's390x'
      AND t.method = 'buildArch'
      AND t.start_ts IS NOT NULL
    GROUP BY 1
)
SELECT CASE
           WHEN tasks <   250 THEN 'a. under 250'
           WHEN tasks <   500 THEN 'b. 250-500'
           WHEN tasks <  1000 THEN 'c. 500-1,000'
           WHEN tasks <  2000 THEN 'd. 1,000-2,000'
           WHEN tasks <  4000 THEN 'e. 2,000-4,000'
           ELSE                    'f. over 4,000'
       END AS tasks_started_that_day,
       count(*) AS days,
       round(avg(mean_wait_s) / 60, 1) AS mean_wait_min,
       round(max(worst_wait_s) / 60, 1) AS worst_wait_min,
       round(avg(build_hours), 1) AS mean_build_hours
FROM per_day
GROUP BY 1 ORDER BY 1;
