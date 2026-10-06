'use strict'
const assert = require('node:assert/strict')
const { createHistoryProvider } = require('../lib/history')
async function main () {
  const { provider, stop } = createHistoryProvider({
    app: { getSelfPath: () => 'urn:test:http' },
    port: Number(new URL(process.argv[2]).port)
  })
  const time = { from: '2020-01-01T00:00:00Z', to: '2020-01-01T00:00:30Z' }
  const methods = ['average', 'min', 'max', 'first', 'last']
  const reply = await provider.getValues({
    ...time, context: 'vessels.self', resolution: 30,
    pathSpecs: methods.map(aggregate => ({ path: 'navigation.speedOverGround', aggregate, parameter: [] }))
  })
  assert.deepEqual(reply, {
    context: 'vessels.urn:test:http',
    range: { from: '2020-01-01T00:00:00.000Z', to: '2020-01-01T00:00:30.000Z' },
    values: methods.map(method => ({ path: 'navigation.speedOverGround', method })),
    data: [['2020-01-01T00:00:00.000Z', 3, 1, 5, 2.5, 4.5]]
  })
  const position = await provider.getValues({
    ...time, resolution: 30,
    pathSpecs: ['first', 'last'].map(aggregate => ({ path: 'navigation.position', aggregate, parameter: [] }))
  })
  assert.deepEqual(position.data, [['2020-01-01T00:00:00.000Z',
    { latitude: 60, longitude: 24 }, { latitude: 61, longitude: 25 }]])
  const buckets = await provider.getValues({
    ...time, resolution: 10,
    pathSpecs: [{ path: 'navigation.speedOverGround', aggregate: 'average', parameter: [] }]
  })
  assert.deepEqual(buckets.data, [
    ['2020-01-01T00:00:10.000Z', 2], ['2020-01-01T00:00:20.000Z', 4]
  ])
  assert.deepEqual(await provider.getContexts(time), ['vessels.urn:test:http'])
  const paths = await provider.getPaths(time)
  for (const path of ['navigation.speedOverGround', 'navigation.position', 'navigation.position.latitude', 'navigation.position.longitude']) {
    assert.ok(paths.includes(path), path)
  }
  stop()
  console.log('Real Lume HTTP History aggregates, tracks, contexts and paths: PASS')
}
main().catch(error => { console.error(error); process.exitCode = 1 })
