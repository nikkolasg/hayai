#!/usr/bin/env python3
"""Tests of scripts/race_sidecar.py with the files of scripts/fixtures/race_blocks
(scripts/test_race_blocks.py describes them).

Usage: python3 -m unittest scripts/test_race_sidecar.py
"""

import os
import pathlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import race_sidecar  # noqa: E402

FIXTURE = pathlib.Path(__file__).resolve().parent / "fixtures" / "race_blocks"


def lines(path, first, last):
    """Lines first to last (from 1) of a file, with their line ends."""
    return "".join(path.read_text().splitlines(keepends=True)[first - 1 : last])


class Sidecar(unittest.TestCase):
    def setUp(self):
        self.dir = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)

    def append(self, name, text):
        with open(self.dir / name, "a") as f:
            f.write(text)

    def test_the_zakura_value_waits_for_the_request_row_and_uses_the_calibration(self):
        peer = FIXTURE / "zakurad-traces" / "legacy_peer_request.jsonl"
        shutil.copy(FIXTURE / "zakurad-traces" / "legacy_sync.jsonl", self.dir)
        shutil.copy(FIXTURE / "zakurad-getblocktemplate.jsonl", self.dir)
        # The rows before the request of the block 103, and the log lines to its commit.
        self.append("legacy_peer_request.jsonl", lines(peer, 1, 11))
        self.append("zakurad.log", lines(FIXTURE / "zakurad-block-lines.log", 1, 6))
        node = race_sidecar.Zakura(self.dir, self.dir / "zakurad.log", self.dir / "zakurad-getblocktemplate.jsonl")
        node.read()
        values = node.values()
        self.assertEqual(values["race_last_block_height"], 103)
        self.assertNotIn("race_last_block_received_to_committed_seconds", values)
        # The first answer on the block 103 comes 1.98 s after the log line.
        self.assertAlmostEqual(values["race_last_block_template_served_seconds"], 1.98, places=6)
        self.assertEqual(values["race_last_block_template_transactions"], 0)
        # Calls without `longpollid`: 1,500 µs, 450 µs, 650 µs and 2,500 µs.
        self.assertEqual(values["race_rpc_getblocktemplate_seconds_count"], 4)
        self.assertAlmostEqual(values["race_rpc_getblocktemplate_seconds_sum"], 0.0051, places=9)

        # The request row of the block 103 comes later than its log line.
        self.append("legacy_peer_request.jsonl", lines(peer, 12, 16))
        node.read()
        values = node.values()
        error = values["race_last_block_clock_error_seconds"]
        self.assertLessEqual(error, 0.0001)
        # The true value of the fixture, inside the error of the calibration plus the 3 µs
        # between a row of legacy_sync and its log line.
        self.assertLessEqual(abs(values["race_last_block_received_to_committed_seconds"] - 0.020), error + 0.000003)

        # The next block: the gauges of the block 103 go away with it.
        self.append("zakurad.log", lines(FIXTURE / "zakurad-block-lines.log", 7, 8))
        node.read()
        values = node.values()
        self.assertEqual(values["race_last_block_height"], 104)
        self.assertNotIn("race_last_block_template_served_seconds", values)

    def test_a_row_of_an_earlier_start_of_zakurad_does_not_count(self):
        node = race_sidecar.Zakura(self.dir, self.dir / "zakurad.log", None)
        self.assertTrue(node.of_this_process({"process_trace_id": "200-1700000100000000000"}))
        node.windows.append((1, 2))
        self.assertFalse(node.of_this_process({"process_trace_id": "100-1700000000000000000"}))
        self.assertEqual(list(node.windows), [(1, 2)])
        # A later start: the calibration starts again.
        self.assertTrue(node.of_this_process({"process_trace_id": "300-1700000200000000000"}))
        self.assertEqual(list(node.windows), [])

    def test_the_hayai_values_are_the_trace_fields_of_the_last_commit(self):
        node = race_sidecar.Hayai(FIXTURE / "hayaid-traces", FIXTURE / "hayaid-getblocktemplate.jsonl")
        node.read()
        values = node.values()
        # The row of the block 105 has the result rejected.
        self.assertEqual(values["race_last_block_height"], 104)
        self.assertEqual(values["race_last_block_received_to_committed_seconds"], 0.00095)
        self.assertNotIn("race_last_block_clock_error_seconds", values)
        # The caller has no answer on the block 104.
        self.assertNotIn("race_last_block_template_served_seconds", values)
        self.assertEqual(values["race_rpc_getblocktemplate_seconds_count"], 4)

    def test_the_cgroup_files_give_the_resources(self):
        (self.dir / "cpu.stat").write_text("usage_usec 2500000\nuser_usec 2000000\nsystem_usec 500000\n")
        (self.dir / "memory.current").write_text("1048576\n")
        (self.dir / "io.stat").write_text(
            "259:0 rbytes=1000 wbytes=2000 rios=1 wios=2 dbytes=0 dios=0\n"
            "253:0 rbytes=1000 wbytes=2000 rios=1 wios=2 dbytes=0 dios=0\n"
            "8:0 rbytes=10 wbytes=20 rios=1 wios=1 dbytes=0 dios=0\n"
        )
        # 253:0 is a device-mapper device on top of 259:0.
        with mock.patch.object(race_sidecar, "stacked", lambda device: device == "253:0"):
            values = race_sidecar.cgroup_values(str(self.dir))
        self.assertEqual(
            values,
            {
                "race_node_cpu_seconds_total": 2.5,
                "race_node_memory_bytes": 1048576,
                "race_node_io_read_bytes_total": 1010,
                "race_node_io_write_bytes_total": 2020,
            },
        )
        self.assertIsNone(race_sidecar.cgroup_values(str(self.dir / "gone")))

    def test_the_data_size_counts_each_inode_one_time(self):
        (self.dir / "a").write_bytes(b"x" * 10000)
        os.link(self.dir / "a", self.dir / "b")
        (self.dir / "sub").mkdir()
        expected = sum(os.lstat(self.dir / n).st_blocks * 512 for n in ("", "a", "sub"))
        self.assertEqual(race_sidecar.disk_bytes(str(self.dir)), expected)

    def test_the_textfile_has_one_help_for_each_metric_with_a_value(self):
        text = race_sidecar.textfile(
            "zakurad",
            {
                "race_last_block_height": 103,
                "race_rpc_getblocktemplate_seconds_sum": 0.5,
                "race_rpc_getblocktemplate_seconds_count": 4,
            },
        )
        self.assertIn('race_last_block_height{node="zakurad"} 103\n', text)
        self.assertIn("# TYPE race_rpc_getblocktemplate_seconds summary\n", text)
        self.assertIn('race_rpc_getblocktemplate_seconds_sum{node="zakurad"} 0.5\n', text)
        self.assertIn('race_rpc_getblocktemplate_seconds_count{node="zakurad"} 4\n', text)
        self.assertEqual(text.count("# HELP "), 2)
        self.assertNotIn("race_node_memory_bytes", text)

    def test_a_partial_line_waits_for_its_end(self):
        tail = race_sidecar.Tail(str(self.dir / "f.jsonl"))
        self.assertEqual(tail.lines(), [])
        self.append("f.jsonl", '{"a": 1}\n{"b"')
        self.assertEqual(tail.lines(), ['{"a": 1}'])
        self.append("f.jsonl", ": 2}\n")
        self.assertEqual(tail.lines(), ['{"b": 2}'])


if __name__ == "__main__":
    unittest.main()
