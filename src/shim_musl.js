#!/usr/bin/env node
'use strict'

const { spawnSync, execSync } = require('child_process')
const { platform, arch } = process

/**
 * Detects whether the system's C library is musl.
 *
 * Executes `ldd --version` and inspects the command output for the substring `musl`.
 * @returns {boolean} `true` if the system uses the musl C library, `false` otherwise.
 */
function isMusl() {
  let stderr
  try {
    stderr = execSync('ldd --version', { stdio: ['pipe', 'pipe', 'pipe'] })
  } catch (err) {
    stderr = err.stderr
  }
  return stderr.indexOf('musl') > -1
}

const PLATFORMS = __PLATFORMS__

const key = platform === 'linux' && isMusl() ? 'linux-musl' : platform
const binPath = PLATFORMS[key]?.[arch]

if (!binPath) {
  console.error(`__NAME__: unsupported platform: ${platform} ${arch}`)
  process.exit(1)
}

const bin = require.resolve(binPath)
const result = spawnSync(bin, process.argv.slice(2), { stdio: 'inherit' })
if (result.error) {
  throw result.error
}
process.exitCode = result.status ?? 1
