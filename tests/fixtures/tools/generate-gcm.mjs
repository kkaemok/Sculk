import {readFileSync,writeFileSync} from 'node:fs';
import {resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
const base=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const reference=process.argv[2];
if(!reference)throw new Error('Provide the exact Agent Messenger checkout path');
import {createCipheriv,createDecipheriv,createHash,createPublicKey} from 'node:crypto';
const source=readFileSync(resolve(reference,'src/platforms/kakaotalk/protocol/config.ts'),'utf8');
const section=source.split('LOCO_RSA_PUBLIC_KEY_DER_B64 =')[1].split('export const BOOKING_HOST')[0];
const der=Buffer.from([...section.matchAll(/'([^']+)'/g)].map(m=>m[1]).join(''),'base64');
createPublicKey({key:der,format:'der',type:'pkcs1'});
console.log('reference RSA PKCS1 DER SHA256',createHash('sha256').update(der).digest('hex'));
// Test vectors use public synthetic key/nonce and contain no account data.
const key=Buffer.alloc(16,7),nonce=Buffer.alloc(12,9),plain=Buffer.from('78563412000050494e470000000000000000050000000500000000','hex');
const c=createCipheriv('aes-128-gcm',key,nonce);const encrypted=Buffer.concat([c.update(plain),c.final(),c.getAuthTag()]);
const length=Buffer.alloc(4);length.writeUInt32LE(nonce.length+encrypted.length);
writeFileSync(resolve(base,'gcm-node-ping.bin'),Buffer.concat([length,nonce,encrypted]));
writeFileSync(resolve(base,'documented-server-public.der'),der);
