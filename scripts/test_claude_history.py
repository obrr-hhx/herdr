import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "claude_history", Path(__file__).parent / "history/claude_history.py"
)
history = importlib.util.module_from_spec(spec)
spec.loader.exec_module(history)


def message(number, text, role="assistant"):
    return {
        "type": role,
        "uuid": f"record-{number}",
        "message": {"role": role, "content": [{"type": "text", "text": text}]},
    }


class HistoryTests(unittest.TestCase):
    def test_pages_recover_first_and_last_records_without_partial_json(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "history.jsonl"
            path.write_text(
                "".join(
                    json.dumps(message(i, f"中文 history {i}"), ensure_ascii=False)
                    + "\n"
                    for i in range(150)
                )
            )
            records = {}
            before = None
            with patch.object(history, "WINDOW", 1000):
                for _ in range(100):
                    page = history.page(path, before)
                    for record in page["records"]:
                        records[record["id"]] = record
                    if not page["has_older"]:
                        break
                    self.assertNotEqual(page["before"], before)
                    before = page["before"]
            self.assertEqual(len(records), 150)
            self.assertEqual(records["record-0:0"]["text"], "中文 history 0")
            self.assertEqual(records["record-149:0"]["text"], "中文 history 149")

    def test_incomplete_tail_is_retried_and_large_blocks_are_explicitly_clipped(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "history.jsonl"
            path.write_text(
                json.dumps(
                    {
                        "type": "user",
                        "uuid": "tool",
                        "message": {
                            "role": "user",
                            "content": [
                                {"type": "tool_result", "content": "x" * 70000}
                            ],
                        },
                    }
                )
                + "\n"
                + '{"message":'
            )
            page = history.page(path)
            self.assertEqual(len(page["records"]), 1)
            self.assertIn("limit", page["records"][0]["text"])

    def test_tool_results_images_and_control_sequences_are_data_only(self):
        value = {
            "type": "user",
            "uuid": "x",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "is_error": True,
                        "content": "\x1b[31merror\x1b[0m",
                    },
                    {"type": "image", "source": {"data": "never-export-base64"}},
                ],
            },
        }
        blocks = history.blocks(value, 0)
        self.assertEqual(blocks[0]["kind"], "error")
        self.assertEqual(blocks[0]["text"], "error")
        self.assertNotIn("never-export-base64", json.dumps(blocks))

    def test_meta_sidechain_and_non_message_records_are_excluded(self):
        for flags in [{"isMeta": True}, {"isSidechain": True}]:
            self.assertEqual(
                history.blocks(
                    {**message(0, "not a main conversation message"), **flags}, 0
                ),
                [],
            )
        self.assertEqual(history.blocks({"type": "progress"}, 0), [])
        self.assertEqual(
            history.blocks(
                {**message(0, "internal request context"), "type": "api-request-blob"},
                0,
            ),
            [],
        )

    def test_missing_or_ambiguous_pid_binding_never_guesses_newest_log(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "sessions").mkdir()
            (root / "projects" / "-repo").mkdir(parents=True)
            identity = "12345678-1234-1234-1234-123456789abc"
            (root / "projects" / "-repo" / f"{identity}.jsonl").write_text("{}\n")
            with self.assertRaises(ValueError):
                history.resolve(root, {"foreground_processes": []})
            # Two sessions reporting for the foreground group are ambiguous, even
            # when one file is newer. Tests use nonexistent PIDs to avoid /proc.
            for pid in (90000001, 90000002):
                (root / "sessions" / f"{pid}.json").write_text(
                    json.dumps({"pid": pid, "sessionId": identity, "cwd": "/repo"})
                )
            with self.assertRaises(ValueError):
                history.resolve(
                    root,
                    {"foreground_processes": [{"pid": 90000001}, {"pid": 90000002}]},
                )


if __name__ == "__main__":
    unittest.main()
