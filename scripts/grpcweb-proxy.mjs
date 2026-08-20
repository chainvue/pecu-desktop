// A grpc-web (HTTP/1.1) front for a native gRPC (HTTP/2) lightwalletd.
//
// # Why this exists
//
// `verus-light` speaks grpc-web over HTTP/1.1 on purpose: no HTTP/2 stack and
// no async runtime, so one transport serves a desktop build and a wasm one.
// lightwalletd speaks native gRPC over HTTP/2. Bridging the two is what a
// grpc-web proxy does, and the SDK's own example points at a local one.
//
// Verus runs no public grpc-web endpoint — measured on 2026-08-20 against
// lightwalletd.verustest.net ports 80, 443, 8080, 8081, 8125 and 9067. Only
// 8125 answers, and it sends an HTTP/2 SETTINGS frame the instant a socket
// opens. So there is nothing to point the wallet at without running this.
//
// Node's `http2` is in the standard library, which is the whole reason this is
// written in JavaScript in a Rust repository: it needs no install, no module
// download and no container. Read it before running it — it is sixty lines.
//
//   node scripts/grpcweb-proxy.mjs                     # 127.0.0.1:8080
//   PORT=9000 UPSTREAM=host:port node scripts/grpcweb-proxy.mjs
//
// # This is a development tool and is not shipped
//
// `insecure` below disables certificate checking on the hop to lightwalletd,
// because Verus's testnet certificate expired on 2026-08-11. That is acceptable
// **here** and nowhere else: this runs on a developer's machine, against a
// testnet, to produce evidence. The wallet itself never relaxes verification —
// `GrpcWebTransport` refuses plaintext to any non-loopback host, and the only
// reason it will talk to this proxy is that 127.0.0.1 is loopback.

import http from 'node:http'
import http2 from 'node:http2'

const PORT = Number(process.env.PORT ?? 8080)
const [host, port] = (process.env.UPSTREAM ?? 'lightwalletd.verustest.net:8125').split(':')
const insecure = process.env.INSECURE !== '0'

/** One grpc-web frame: a flag byte, a big-endian length, then the payload. */
function frame(flag, payload) {
  const head = Buffer.alloc(5)
  head[0] = flag
  head.writeUInt32BE(payload.length, 1)
  return Buffer.concat([head, payload])
}

/** grpc-web carries trailers as a frame with the top bit set. */
function trailerFrame(trailers) {
  const text = Object.entries(trailers)
    .filter(([name]) => name.startsWith('grpc-'))
    .map(([name, value]) => `${name}:${value}`)
    .join('\r\n')
  return frame(0x80, Buffer.from(text + '\r\n', 'utf8'))
}

const upstream = `https://${host}:${port}`

http
  .createServer((request, response) => {
    const chunks = []
    request.on('data', (chunk) => chunks.push(chunk))
    request.on('end', () => {
      const session = http2.connect(upstream, { rejectUnauthorized: !insecure })
      session.on('error', (error) => {
        response.writeHead(502, { 'content-type': 'text/plain' })
        response.end(`upstream: ${error.message}\n`)
      })

      const stream = session.request({
        ':method': 'POST',
        ':path': request.url,
        // Native gRPC, which is what the far end speaks. The framing inside the
        // body is identical to grpc-web's, so the payload passes through
        // untouched — only the envelope changes.
        'content-type': 'application/grpc+proto',
        te: 'trailers',
      })

      const body = []
      let trailers = {}
      stream.on('response', () => {})
      stream.on('trailers', (received) => {
        trailers = received
      })
      stream.on('data', (chunk) => body.push(chunk))
      stream.on('end', () => {
        session.close()
        response.writeHead(200, {
          'content-type': 'application/grpc-web+proto',
          // Named here as well as in the trailer frame: the SDK reads whichever
          // it finds, and a reply carrying neither is a framing error.
          'grpc-status': trailers['grpc-status'] ?? '0',
        })
        response.end(Buffer.concat([...body, trailerFrame(trailers)]))
      })
      stream.on('error', (error) => {
        session.close()
        response.writeHead(502, { 'content-type': 'text/plain' })
        response.end(`stream: ${error.message}\n`)
      })

      stream.end(Buffer.concat(chunks))
    })
  })
  .listen(PORT, '127.0.0.1', () => {
    console.log(`grpc-web  http://127.0.0.1:${PORT}  ->  ${upstream}  (insecure=${insecure})`)
  })
