// Independent Node implementation of the wire protocol; never contacts upstream.
import http from 'node:http';
import { createCipheriv, createDecipheriv, createECDH, hkdfSync, randomBytes } from 'node:crypto';

const sessions = new Map();
let helloCount = 0;
let expiredOnce = false;
const timestamp = () => Math.floor(Date.now() / 1000) + 120;
const server = http.createServer(async (req, res) => {
  const send = (status, body) => {
    res.writeHead(status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(body));
  };
  try {
    if (req.url?.startsWith('/fixture/')) {
      if (req.url === '/fixture/redirect.m3u8') {
        res.writeHead(302, { Location: `http://localhost:${server.address().port}/fixture/master.m3u8` });
        return res.end();
      }
      if (req.url.endsWith('.m3u8')) {
        if (req.headers['x-fixture-auth'] !== 'fixture-secret' || req.headers['x-cdn-app'] !== 'fixture-app') return send(403, {});
        res.writeHead(200, { 'Content-Type': 'application/vnd.apple.mpegurl' });
        return res.end(req.url.endsWith('master.m3u8')
          ? '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000\nvariant.m3u8\n'
          : '#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI="key.key"\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n');
      }
      if (req.headers['x-fixture-auth']) return send(400, { error: 'Unexpected segment credentials' });
      res.writeHead(200, { 'Content-Type': 'application/octet-stream' });
      return res.end(Buffer.alloc(req.url.endsWith('.key') ? 16 : 188, 1));
    }
    if (req.url !== '/api/playback/request' || req.method !== 'POST') return send(404, {});
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const packet = JSON.parse(Buffer.concat(chunks).toString());
    if (req.headers.authorization) throw new Error('Bearer must remain inside encryption');
    if (packet.type === 'hello') {
      helloCount++;
      const ecdh = createECDH('prime256v1');
      const publicKey = ecdh.generateKeys();
      const shared = ecdh.computeSecret(Buffer.from(packet.client_pub, 'base64url'));
      const salt = randomBytes(32);
      const hello = { v: 1, type: 'hello', kid: 'fixture', sid: randomBytes(24).toString('base64url'),
        site: 'fixture.test', server_time: timestamp(), expires_at: timestamp() + 120,
        server_pub: publicKey.toString('base64url'), salt: salt.toString('base64url') };
      const info = ['dx150-edge-v1', 'playback', hello.site, hello.kid, hello.sid].join('\n');
      const key = direction => Buffer.from(hkdfSync('sha256', shared, salt, info + '\n' + direction, 32));
      sessions.set(hello.sid, { info, requestKey: key('req'), responseKey: key('res'), sequences: new Set() });
      return send(200, hello);
    }
    const session = sessions.get(packet.sid);
    if (!session) return send(401, { error: 'session_expired' });
    if (packet.kid !== 'fixture' || !Number.isInteger(packet.seq) || packet.seq < 1 || packet.seq > 16384
      || session.sequences.has(packet.seq) || Math.abs(packet.ts - timestamp()) > 5) throw new Error('Invalid sequence or clock');
    session.sequences.add(packet.seq);
    const iv = Buffer.alloc(12);
    iv.writeUInt32BE(packet.seq, 8);
    const aad = direction => Buffer.from([session.info, direction, packet.seq, packet.ts].join('\n'));
    const encrypted = Buffer.from(packet.ciphertext, 'base64url');
    const decoder = createDecipheriv('aes-256-gcm', session.requestKey, iv);
    decoder.setAAD(aad('req'));
    decoder.setAuthTag(encrypted.subarray(-16));
    const inner = JSON.parse(Buffer.concat([decoder.update(encrypted.subarray(0, -16)), decoder.final()]).toString());
    const url = new URL(inner.request.path, 'https://fixture.test');
    const scenario = url.searchParams.get('scenario');
    if (scenario === 'expired' && !expiredOnce) {
      expiredOnce = true;
      sessions.delete(packet.sid);
      return send(401, { error: 'session_expired' });
    }
    const body = { status: 1, data: { video_url: 'https://media.example.test/test.m3u8', ep_num: 1,
      diagnostics: { helloCount, sequence: packet.seq, tokenSeen: inner.authorization === 'Bearer fixture-token' } } };
    const response = { status: scenario === 'forbidden' ? 403 : 200,
      headers: { 'Content-Type': ['application/json'] }, body: Buffer.from(JSON.stringify(body)).toString('base64') };
    const encoder = createCipheriv('aes-256-gcm', session.responseKey, iv);
    encoder.setAAD(aad('res'));
    const ciphertext = Buffer.concat([encoder.update(JSON.stringify(response)), encoder.final(), encoder.getAuthTag()]);
    if (scenario === 'tamper') ciphertext[0] ^= 1;
    send(200, { v: 1, type: 'result', kid: packet.kid, sid: packet.sid,
      seq: scenario === 'mismatch' ? packet.seq + 1 : packet.seq, ts: packet.ts,
      ciphertext: ciphertext.toString('base64url') });
  } catch (error) {
    send(400, { error: error.message });
  }
});
server.listen(0, '127.0.0.1', () => console.log(`http://127.0.0.1:${server.address().port}`));
