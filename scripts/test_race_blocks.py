#!/usr/bin/env python3
"""Tests of scripts/race_blocks.py with the files of scripts/fixtures/race_blocks.

Usage: python3 -m unittest scripts/test_race_blocks.py

The fixture has the blocks 100 to 104 of hayaid. zakurad has the blocks 101 to 103 with
the same hashes, and another block at the height 104. The clock of legacy_sync starts
100,000 µs before the first row, and the clock of legacy_peer_request 250,000 µs after
that. The true "received to committed" times of zakurad are 4,000 µs, 6,000 µs and
20,000 µs. The rows of the fixture leave a range of 150 µs for the second clock.

The files of the caller (scripts/race_rpc_caller.py): hayaid answers on the block 101 by
long poll 400 µs after its commit and on the block 102 by a call without `longpollid`
1.995 s after its commit, and has no answer on the blocks 103 and 104. zakurad answers
on the block 101 by long poll 1,500 µs after its log line, on the block 102 100 µs before
its log line, and on the block 103 by a call without `longpollid` 1.98 s after its log
line. Each file has one error line.
"""

import csv
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import race_blocks  # noqa: E402

FIXTURE = pathlib.Path(__file__).resolve().parent / "fixtures" / "race_blocks"


class RaceBlocks(unittest.TestCase):
    def test_hayai_rows_come_from_the_trace_fields(self):
        blocks = race_blocks.hayai_blocks(FIXTURE / "hayaid-traces")
        self.assertEqual(sorted(blocks), [100, 101, 102, 103, 104])
        block = blocks[101]
        self.assertEqual(block["hash"], "65" * 32)
        self.assertEqual(block["source"], "download")
        self.assertEqual((block["transactions"], block["bytes"]), (3, 1101))
        self.assertEqual(block["received_to_validated"], 0.0012)
        self.assertEqual(block["received_to_committed"], 0.002)
        self.assertEqual(block["contextual_commit"], 0.0003)
        # The first template of each kind on the block.
        self.assertEqual((block["template_empty"], block["template_full"]), (0.0003, 0.0009))
        self.assertNotIn("template_empty", blocks[102])
        self.assertEqual(blocks[102]["template_full"], 0.0018)

    def test_the_zakura_clock_is_calibrated_inside_its_stated_error(self):
        values, calibration = race_blocks.zakura_received_to_committed(
            FIXTURE / "zakurad-traces", FIXTURE / "zakurad-block-lines.log"
        )
        self.assertEqual(calibration["sync_rounds"], 4)
        self.assertEqual(calibration["find_rows_in_a_round"], calibration["find_rows"])
        self.assertAlmostEqual(calibration["error_s"], 0.000075, places=9)
        truth = {"65" * 32: (101, 0.004), "66" * 32: (102, 0.006), "67" * 32: (103, 0.020)}
        for block_hash, (height, expected) in truth.items():
            got_height, got = values[block_hash]
            self.assertEqual(got_height, height)
            # The error of the calibration, plus the 3 µs between a row of legacy_sync
            # and its log line in the fixture.
            self.assertLessEqual(abs(got - expected), calibration["error_s"] + 0.000003)

    def test_no_zakura_value_without_a_calibration(self):
        with tempfile.TemporaryDirectory() as empty:
            values, calibration = race_blocks.zakura_received_to_committed(
                empty, FIXTURE / "zakurad-block-lines.log"
            )
        self.assertEqual(values, {})
        self.assertIsNone(calibration["error_s"])

    def test_the_contextual_commit_of_zakura_is_the_increase_of_one_block(self):
        values, stats = race_blocks.zakura_contextual_commit([FIXTURE / "zakurad-series-0.json"])
        self.assertEqual(sorted(values), [101, 102])
        self.assertAlmostEqual(values[101], 0.003, places=9)
        self.assertAlmostEqual(values[102], 0.007, places=9)
        # The blocks 103 and 104 are in one scrape interval: no value.
        self.assertEqual(stats, {"blocks": 2, "shared_intervals": 1, "no_height": 0})

    def test_the_table_has_one_row_for_each_height_at_the_tip(self):
        with tempfile.TemporaryDirectory() as out:
            out = pathlib.Path(out)
            subprocess.run(
                [
                    sys.executable,
                    str(pathlib.Path(race_blocks.__file__)),
                    "--hayai-traces",
                    str(FIXTURE / "hayaid-traces"),
                    "--zakura-traces",
                    str(FIXTURE / "zakurad-traces"),
                    "--zakura-log",
                    str(FIXTURE / "zakurad-block-lines.log"),
                    "--zakura-series",
                    str(FIXTURE / "zakurad-series-0.json"),
                    "--hayai-caller",
                    str(FIXTURE / "hayaid-getblocktemplate.jsonl"),
                    "--zakura-caller",
                    str(FIXTURE / "zakurad-getblocktemplate.jsonl"),
                    "--out-csv",
                    str(out / "blocks.csv"),
                    "--out-md",
                    str(out / "blocks.md"),
                ],
                check=True,
                capture_output=True,
            )
            with open(out / "blocks.csv", newline="") as f:
                rows = list(csv.DictReader(f))
            summary = (out / "blocks.md").read_text()
        # The table starts at the first block that zakurad got by gossip.
        self.assertEqual([row["height"] for row in rows], ["101", "102", "103", "104"])
        self.assertEqual(list(rows[0]), race_blocks.COLUMNS)
        first = rows[0]
        self.assertEqual(first["hayai_received_to_committed_s"], "0.002000")
        zakura = float(first["zakura_received_to_committed_s"])
        self.assertLessEqual(abs(zakura - 0.004), 0.000078)
        self.assertAlmostEqual(float(first["diff_received_to_committed_s"]), 0.002 - zakura, places=6)
        self.assertEqual(first["hayai_contextual_commit_s"], "0.000300")
        self.assertEqual(first["zakura_contextual_commit_s"], "0.003000")
        self.assertEqual(first["diff_contextual_commit_s"], "-0.002700")
        self.assertEqual(first["hayai_received_to_template_empty_s"], "0.000300")
        # No invented value: an empty cell where a node has no measurement.
        self.assertEqual(rows[1]["hayai_received_to_template_empty_s"], "")
        self.assertEqual(rows[2]["zakura_contextual_commit_s"], "")
        self.assertEqual(rows[2]["diff_contextual_commit_s"], "")
        # zakurad has another block at the height 104.
        self.assertEqual(rows[3]["zakura_received_to_committed_s"], "")
        self.assertEqual(rows[3]["note"], "zakurad has another block at this height")
        # The first answer of each caller on the block, minus the commit on that machine.
        self.assertEqual(
            [row["hayai_template_served_s"] for row in rows], ["0.000400", "1.995000", "", ""]
        )
        self.assertEqual(
            [row["zakura_template_served_s"] for row in rows], ["0.001500", "-0.000100", "1.980000", ""]
        )
        self.assertIn("Error of each Zakura value from the clock calibration: 0.000075 s", summary)
        # Calls without `longpollid`, their mean time, long poll answers, errors, and the
        # mode of the first answer on a block of the table.
        self.assertIn("| hayaid | 3 | 0.000400 | 2 | 1 | 1 | 1 |", summary)
        self.assertIn("| zakurad | 2 | 0.002000 | 2 | 1 | 2 | 1 |", summary)
        self.assertIn("Scrape intervals with 2 or more blocks (no value): 1.", summary)

    def test_the_caller_file_gives_the_first_answer_on_each_block(self):
        answers, stats = race_blocks.caller_answers(FIXTURE / "hayaid-getblocktemplate.jsonl")
        # The later answers on the block 101 (a template change, a call) do not count.
        self.assertEqual(answers["65" * 32], (1700000075003400, "longpoll"))
        self.assertEqual(answers["66" * 32], (1700000152000000, "poll"))
        self.assertEqual(
            stats, {"polls": 3, "long_polls": 2, "errors": 1, "mean_poll_s": 0.0004}
        )

    def test_no_template_served_value_without_a_caller_file(self):
        hayai = race_blocks.hayai_blocks(FIXTURE / "hayaid-traces")
        rows = race_blocks.build_rows(hayai, {}, {}, None)
        self.assertTrue(all(row["hayai_template_served_s"] is None for row in rows))
        self.assertTrue(all(row["zakura_template_served_s"] is None for row in rows))
        self.assertIn("No file of the caller", race_blocks.summary(rows, None, None, None))

    def test_from_height_selects_the_tip_phase(self):
        hayai = race_blocks.hayai_blocks(FIXTURE / "hayaid-traces")
        rows = race_blocks.build_rows(hayai, {}, {}, 103)
        self.assertEqual([row["height"] for row in rows], [103, 104])
        self.assertTrue(all(row["zakura_received_to_committed_s"] is None for row in rows))


if __name__ == "__main__":
    unittest.main()
