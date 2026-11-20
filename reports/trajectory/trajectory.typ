// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// sandogasa's development trajectory, release by release (#20), drawn
// from the JSON reports/trajectory/trajectory.py writes:
//
//   typst compile --input data=trajectory.json trajectory.typ
//
// Plain Typst shapes, no packages, so a build needs no network.

#let data = json(sys.inputs.at("data", default: "trajectory.json"))
#let rows = data.releases
#let last = rows.last()

#set document(title: "sandogasa: development trajectory")
#set page(paper: "a4", flipped: true, margin: 1.5cm, numbering: "1")
#set text(size: 11pt)
#set par(justify: false)
#show heading: set block(sticky: true)

// Colors that read in print and on screen.
#let code-color = rgb("#3b6ea8")
#let test-color = rgb("#8fb5d9")
#let other-color = rgb("#c9c9c9")
#let added-color = rgb("#4c9a5b")
#let removed-color = rgb("#c0504d")
#let accent = rgb("#d08c2a")

#let thousands(n) = {
  let s = str(calc.round(n))
  let out = ""
  let i = 0
  for c in s.rev().clusters() {
    if i > 0 and calc.rem(i, 3) == 0 { out = "," + out }
    out = c + out
    i += 1
  }
  out
}
#let dollars(n) = "$" + thousands(n)

// Round tick marks for an axis from 0 to `peak`: a step of 1, 2, 2.5
// or 5 times a power of ten giving about five ticks, and the axis top,
// the first tick at or above `peak`.
#let ticks(peak) = {
  let rough = peak / 5
  let mag = calc.pow(10.0, calc.floor(calc.log(rough, base: 10)))
  let step = (1, 2, 2.5, 5, 10).map(m => m * mag).find(s => s >= rough)
  let last = calc.ceil(peak / step) * step
  (step: step, top: last, marks: range(int(calc.round(last / step)) + 1).map(k => k * step))
}

// A bar per release, stacked from `parts` (each a (color, value) pair
// list per row), on an axis with round ticks and light gridlines, with
// every `every`-th tag named.
#let bars(rows, parts, height: 3.6cm, every: 4, unit: v => thousands(v)) = {
  let totals = rows.map(r => parts(r).map(p => p.at(1)).sum())
  let axis = ticks(calc.max(..totals, 1e-9))
  let axis-top = axis.top
  let n = rows.len()
  block(width: 100%, breakable: false, {
    grid(
      columns: (2.6cm, 1fr),
      column-gutter: 4pt,
      box(width: 100%, height: height, {
        for mark in axis.marks {
          place(bottom + right, dy: -height * mark / axis-top + 0.5em,
            text(size: 9pt, unit(mark)))
        }
      }),
      box(width: 100%, height: height, stroke: (left: 0.5pt, bottom: 0.5pt), {
        for mark in axis.marks.slice(1) {
          place(bottom + left, dy: -height * mark / axis-top,
            line(length: 100%, stroke: 0.4pt + luma(220)))
        }
        let w = 100% / n
        for (i, r) in rows.enumerate() {
          let y = 0pt
          for (color, value) in parts(r) {
            let h = height * value / axis-top
            place(bottom + left, dx: w * i + w * 0.1, dy: -y,
              rect(width: w * 0.8, height: h, fill: color, stroke: none))
            y += h
          }
        }
      }),
    )
    grid(
      columns: (2.6cm, 1fr),
      column-gutter: 4pt,
      [],
      box(width: 100%, height: 1.1cm, {
        let w = 100% / n
        for (i, r) in rows.enumerate() {
          if calc.rem(i, every) == 0 or i == n - 1 {
            place(top + left, dx: w * i + w * 0.3, dy: 2pt,
              rotate(50deg, origin: top + left, reflow: false,
                text(size: 8.5pt, r.tag)))
          }
        }
      }),
    )
  })
}

#let legend(..items) = {
  for (color, label) in items.pos() {
    box(rect(width: 9pt, height: 9pt, fill: color, stroke: none))
    h(3pt)
    text(size: 10pt, label)
    h(10pt)
  }
}

= sandogasa: development trajectory

#grid(
  columns: (1fr, 1fr),
  column-gutter: 1cm,
  [
    #let releases = rows.filter(r => r.tag != "HEAD")
    Measured at each release tag, #rows.first().tag (#rows.first().date) to
    #releases.last().tag (#releases.last().date): #releases.len() releases#if last.tag == "HEAD" [, and the tree at HEAD (#last.date)].

    At #last.tag the tree holds *#thousands(last.code)* lines of code
    (tokei), *#thousands(last.rust_code)* of them Rust and
    *#thousands(last.rust_tests)* Rust tests, in #last.crates library
    crates and #last.tools tools.

    Basic COCOMO (organic) puts that at
    *#calc.round(last.cocomo.effort_person_months)* person-months over
    #calc.round(last.cocomo.schedule_months, digits: 1) months, about
    *#dollars(last.cocomo.cost)*.
  ],
  [
    #set text(size: 10pt)
    *Reading these numbers.* COCOMO's estimate assumes a human team working
    at a human pace. Next to the actual days between releases, it measures
    this project's AI-assisted pace, not its value.

    Test lines are the Rust code in each file's `#[cfg(test)]` module and
    in `tests/` directories; tokei counts them as code, and so does COCOMO.

    Commit times are when changes were committed (with an assistant, when
    they were approved), not when the work was done. Each author's commits
    are read in the offset most of their commits carry that day.
  ],
)

== Size per release

#legend((code-color, "Rust code"), (test-color, "Rust tests"), (other-color, "everything else"))
#bars(rows, r => (
  (code-color, r.rust_code),
  (test-color, r.rust_tests),
  (other-color, r.code - r.rust_code - r.rust_tests),
))

== Estimated cost (basic COCOMO)

#bars(rows, r => ((accent, r.cocomo.cost),), unit: dollars)

== Change per release

#legend((added-color, "lines added"), (removed-color, "lines removed"))
#bars(rows, r => ((added-color, r.lines_added), (removed-color, r.lines_removed)))

== Estimated cost added per day between releases

#let per-day = rows.enumerate().map(((i, r)) => {
  if i == 0 or r.days_since_previous == none { 0 } else {
    let added = r.cocomo.cost - rows.at(i - 1).cocomo.cost
    // Releases a few hours apart would read as huge rates: a day at least.
    calc.max(added, 0) / calc.max(r.days_since_previous, 1)
  }
})
#bars(rows.enumerate().map(((i, r)) => (tag: r.tag, v: per-day.at(i))),
  r => ((accent, r.v),), unit: dollars)

#let share(part, whole) = if whole == 0 { 0 } else { part / whole }
#let percent = v => str(calc.round(v * 100)) + "%"

// The section's heading, introduction and first chart stay together.
#block(breakable: false)[
  == When commits were made

  Share of each release's commits made at the weekend, and in the evening
  (#data.bands.evening.at(0):00–24:00) or at night
  (00:00–#data.bands.night.at(1):00), local time, of the commits by
  #if data.authors.len() == 0 [every author] else [#data.authors.join(", ")].
  These are when changes were committed, not when the work was done.

  === At the weekend
  #bars(rows, r => ((accent, share(r.commit_times.weekend, r.commit_times.commits)),),
    height: 2.8cm, unit: percent)
]

=== In the evening or at night
#bars(rows, r => ((code-color, share(r.commit_times.evening + r.commit_times.night,
  r.commit_times.commits)),), height: 2.8cm, unit: percent)

#if "heatmap" in data {
  pagebreak()
  [== Commits by weekday and hour (private)]
  let counts = data.heatmap.counts
  let peak = calc.max(..counts.map(row => calc.max(..row)), 1)
  table(
    columns: (1.2cm,) + (1fr,) * 24,
    inset: 5pt,
    stroke: 0.3pt + white,
    align: center,
    [], ..range(24).map(h => text(size: 9pt, str(h))),
    ..data.heatmap.weekdays.zip(counts).map(((day, row)) => (
      text(size: 9pt, day),
      ..row.map(c => table.cell(
        fill: code-color.transparentize(100% - 100% * c / peak),
        text(size: 8pt, fill: if c / peak > 0.5 { white } else { black },
          if c > 0 { str(c) } else { "" }),
      )),
    )).flatten(),
  )
}

#pagebreak()

== Every release

#set text(size: 10pt)
#table(
  columns: (auto, auto) + (1fr,) * 9,
  inset: (x: 4pt, y: 3pt),
  stroke: (x, y) => if y == 0 { (bottom: 0.5pt) } else { none },
  align: (x, y) => if x < 2 { left } else { right },
  [*Tag*], [*Date*], [*Days*], [*Commits*], [*+ lines*], [*− lines*],
  [*Code*], [*Rust*], [*Tests*], [*Crates / tools*], [*COCOMO cost*],
  ..rows.map(r => (
    r.tag, r.date,
    if r.days_since_previous == none { "" } else { str(calc.round(r.days_since_previous, digits: 1)) },
    str(r.commits), thousands(r.lines_added), thousands(r.lines_removed),
    thousands(r.code), thousands(r.rust_code), thousands(r.rust_tests),
    str(r.crates) + " / " + str(r.tools), dollars(r.cocomo.cost),
  )).flatten(),
)
