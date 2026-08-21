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
//   INSECURE=1 node scripts/grpcweb-proxy.mjs          # accept an expired cert
//   UPSTREAM=http://127.0.0.1:9077 node scripts/grpcweb-proxy.mjs
//   PORT=9000 UPSTREAM=host:port node scripts/grpcweb-proxy.mjs
//
// # This is a development tool and is not shipped
//
// It **verifies the upstream certificate by default**. `INSECURE=1` turns that
// off, and is needed today only because lightwalletd.verustest.net is serving a
// certificate that expired on 2026-08-11 — the wildcard was renewed on Aug 7
// and is live on every other Verus host, so this stops being necessary the
// moment that one service reloads.
//
// The default is the safe one deliberately. A tool that lives in a repository
// and skips certificate checks unless told otherwise is a tool somebody will
// eventually point at something that matters, having never read this comment.
//
// What relaxing it costs, so the decision is informed: a man in the middle
// could then feed this proxy a fabricated chain. That cannot move money — the
// spending key never leaves the machine and every transaction is signed locally
// against a recipient and an amount the person typed — but it can show a wrong
// balance, and it can produce a witness anchored to a chain that does not
// exist, which the daemon rejects after the proof has been paid for.
//
// The wallet itself never relaxes anything. `GrpcWebTransport` refuses plaintext
// to any non-loopback host, and the only reason it will talk to this proxy at
// all is that 127.0.0.1 is loopback.

import http from 'node:http'
import http2 from 'node:http2'

const PORT = Number(process.env.PORT ?? 8080)
// `UPSTREAM` may carry a scheme. Without one, https is assumed — a bare
// host:port is almost always somebody else's server across the open internet,
// and defaulting that to cleartext would be the wrong way round.
//
// `http://` selects cleartext HTTP/2, which is what lightwalletd serves on
// loopback: behind a tunnel it has no reason to hold a certificate, and
// `--no-tls-very-insecure` is the ordinary way to start it there. That hop is
// localhost to localhost, with nothing on the wire to protect.
const rawUpstream = process.env.UPSTREAM ?? 'lightwalletd.verustest.net:8125'
const upstream = /^https?:\/\//.test(rawUpstream) ? rawUpstream : `https://${rawUpstream}`
const cleartext = upstream.startsWith('http://')
const insecure = process.env.INSECURE === '1'

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

http
  .createServer((request, response) => {
    const chunks = []
    request.on('data', (chunk) => chunks.push(chunk))
    request.on('end', () => {
      // One reply per request, whoever notices the failure first.
      //
      // A refused certificate raises on the session *and* on the stream, and
      // an earlier version answered both — the second `writeHead` threw
      // `ERR_HTTP_HEADERS_SENT` and took the whole proxy down with it. So a
      // proxy that had just correctly rejected a bad certificate then died,
      // which is a worse failure than the one it was reporting.
      let answered = false
      const fail = (where, error) => {
        if (answered) return
        answered = true
        console.error(`${where}: ${error.message}`)
        response.writeHead(502, { 'content-type': 'text/plain' })
        response.end(`${where}: ${error.message}\n`)
      }

      // `rejectUnauthorized` is meaningless without TLS, and passing it for a
      // cleartext session would suggest a check that is not happening.
      const session = cleartext
        ? http2.connect(upstream)
        : http2.connect(upstream, { rejectUnauthorized: !insecure })
      session.on('error', (error) => fail('upstream', error))

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
        if (answered) return
        answered = true
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
        fail('stream', error)
      })

      stream.end(Buffer.concat(chunks))
    })
  })
  .listen(PORT, '127.0.0.1', () => {
    const how = cleartext ? 'cleartext h2' : insecure ? 'TLS, unverified' : 'TLS, verified'
    console.log(`grpc-web  http://127.0.0.1:${PORT}  ->  ${upstream}  (${how})`)
  })
