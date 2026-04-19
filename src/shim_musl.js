#!/usr/bin/env node
'use strict'

function isMusl() {
  try {
    return require('fs').readFileSync('/usr/bin/ldd', 'utf-8').includes('musl')
  } catch {}
  try {
    const orig = process.report.excludeNetwork
    process.report.excludeNetwork = true
    const report = process.report.getReport()
    process.report.excludeNetwork = orig
    if (report.header?.glibcVersionRuntime) return false
    return report.sharedObjects.some((f) => f.includes('libc.musl-') || f.includes('ld-musl-'))
  } catch {}
}

const PLATFORMS = __PLATFORMS__

const key = process.platform === 'linux' && isMusl() ? 'linux-musl' : process.platform
const binPath = PLATFORMS[key]?.[process.arch]

if (!binPath) {
  console.error(`Unsupported platform: ${process.platform} ${process.arch}`)
  process.exit(1)
}

const bin = require.resolve(binPath)
const result = require('child_process').spawnSync(bin, process.argv.slice(2), { stdio: 'inherit' })
if (result.error) {
  throw result.error
}
process.exitCode = result.status ?? 1
