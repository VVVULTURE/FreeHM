# Generates assets/icons/freehm.ico and freehm.png with the same drawing as crates/freehm/src/icon.rs.
# Usage: python tools/mkicon.py assets/icons/freehm.ico assets/icons/freehm.png
import struct, zlib, sys
def px(size):
    s=float(size); r=s*0.2; out=bytearray(size*size*4)
    for y in range(size):
        for x in range(size):
            fx,fy=x+0.5,y+0.5
            cx=min(max(fx,r),s-r); cy=min(max(fy,r),s-r)
            d=((fx-cx)**2+(fy-cy)**2)**0.5
            a=min(max(r-d+0.5,0.0),1.0)
            if a<=0: continue
            c=(28,30,34)
            for x0,x1,h in [(0.18,0.34,0.35),(0.42,0.58,0.55),(0.66,0.82,0.75)]:
                if s*x0<=fx<s*x1 and s*(0.85-h)<=fy<s*0.85: c=(118,185,0)
            i=(y*size+x)*4
            out[i:i+4]=bytes([c[0],c[1],c[2],int(a*255)])
    return bytes(out)
def png(size):
    raw=px(size); rows=b''.join(b'\0'+raw[y*size*4:(y+1)*size*4] for y in range(size))
    def chunk(t,d): return struct.pack('>I',len(d))+t+d+struct.pack('>I',zlib.crc32(t+d)&0xffffffff)
    return b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',size,size,8,6,0,0,0))+chunk(b'IDAT',zlib.compress(rows,9))+chunk(b'IEND',b'')
sizes=[16,20,24,32,40,48,64,256]
imgs=[png(s) for s in sizes]
hdr=struct.pack('<HHH',0,1,len(sizes)); off=6+16*len(sizes); ents=b''
for s,d in zip(sizes,imgs):
    ents+=struct.pack('<BBBBHHII',s%256,s%256,0,0,1,32,len(d),off); off+=len(d)
open(sys.argv[1],'wb').write(hdr+ents+b''.join(imgs))
open(sys.argv[2],'wb').write(imgs[-1])
