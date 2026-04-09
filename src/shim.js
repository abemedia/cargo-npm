#!/usr/bin/env node
'use strict'

const { spawnSync } = require('child_process')
const { platform, arch } = process

const PLATFORMS = __PLATFORMS__

const binPath = PLATFORMS[platform]?.[arch]

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
