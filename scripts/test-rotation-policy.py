#!/usr/bin/env python3
"""Run the production pure rotation policy without GTK/Tauri dependencies."""
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
source = (root / 'src-tauri/src/services/codex_account_rotation.rs').read_text()
policy = source[source.index('#[derive(Debug, Clone, Default, PartialEq)]\npub struct UsageSnapshot'):source.index('#[derive(Debug, Clone, Serialize, Default)]')]
tests = source[source.index('#[cfg(test)]\nmod tests {'):]
stubs = '''
use std::collections::HashMap;
#[derive(Default)]
pub struct CodexAccountRotation {
    pub enabled: bool,
    pub anchor_provider_id: String,
    pub accounts: Vec<CodexRotationAccount>,
}
pub struct CodexRotationAccount {
    pub account_id: String,
    pub login: String,
    pub priority: u8,
    pub five_hour_min_remaining: u8,
    pub weekly_min_remaining: u8,
    pub heartbeat_eligible: bool,
}
'''
with tempfile.TemporaryDirectory(prefix='rotation-policy-') as directory:
    file = Path(directory) / 'policy.rs'
    binary = Path(directory) / 'policy-tests'
    file.write_text(stubs + policy + tests)
    subprocess.run(['rustc', '--edition=2021', '--test', str(file), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
