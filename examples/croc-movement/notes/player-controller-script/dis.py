import struct,sys
RAM=sys.argv[1]; r=open(RAM,'rb').read()
BASE=int(sys.argv[2],16); LEN=struct.unpack_from('<I',r,(BASE-4)&0x1fffff)[0]
def b(a): return r[a&0x1fffff]
def u16(a): return struct.unpack_from('<H',r,a&0x1fffff)[0]
def s16(a): return struct.unpack_from('<h',r,a&0x1fffff)[0]
def i32(a): return struct.unpack_from('<i',r,a&0x1fffff)[0]
PROP={0:'f38',1:'surf3c',2:'x',3:'y',4:'z',5:'angX',6:'angY',7:'angZ',8:'p8(b4<<4)',9:'p9(b8<<4)',0xa:'p10',0xb:'distMoved',0xc:'PAD',0xd:'PAD_PRESSED',0xe:'PAD_RELEASED',
 0xf:'player.x',0x10:'player.y',0x11:'player.z',0x13:'player.angY',0x16:'f74s',0x17:'f76',0x18:'flag174&200(JUMPING)',0x19:'flag174&400(FALLING)',0x1a:'flag174&8(ANIMPLAYING)',0x1b:'byte3d',0x1c:'f3d0<<4',
 0x23:'f70',0x24:'flag174&400000',0x2d:'camAngY',0x2f:'flag178&200',0x31:'player.f174&200',0x32:'player.f174&400',0x41:'flag178&2(ONGROUND)',0x42:'g75260',0x45:'g75338',0x46:'g751fc',0x47:'ONE',0x48:'MOVESTATE(751cc)',0x49:'TURNVEL(751d0)',
 0x4a:'f b4',0x4b:'f b8',0x4c:'f ba',0x4d:'ONE',0x4e:'flag178&800',0x50:'g75204(clr)',0x52:'g751e0',0x56:'lvl+10',0x57:'g74b4c',0x58:'g74b50'}
def fx(v):
    if v & 0xffff==0: return str(v>>16)
    return f"{v/65536:.4f}({v:#x})"
BIN={6:'+',7:'-',8:'/',9:'*',0xa:'&',0xb:'|',0xc:'==',0xd:'!=',0xe:'>',0xf:'<',0x10:'>=',0x11:'<=',0x20:'&&',0x24:'>>',0x25:'<<'}
UN={0x14:'sin',0x15:'cos',0x17:'sqrt',0x18:'abs',0x19:'rand',0x1a:'int',0x1e:'neg',0x1f:'not',0x1b:'playerWithin',0x21:'f3d0within',0x22:'dist3',0x27:'bonex',0x28:'boney',0x29:'bonez',0x2e:'tbl7c600'}
def expr(a):
    st=[]
    while True:
        o=b(a); a+=1
        if o==1: st.append(f"pvar{u16(a)}"); a+=2
        elif o==2: st.append(f"L{u16(a)}"); a+=2
        elif o==3: p=u16(a); st.append(PROP.get(p,f"prop{p:#x}")); a+=2
        elif o==4: st.append(fx(i32(a))); a+=4
        elif o in BIN:
            y=st.pop() if st else '?'; x=st.pop() if st else '?'; st.append(f"({x} {BIN[o]} {y})")
        elif o in UN: x=st.pop() if st else '?'; st.append(f"{UN[o]}({x})")
        elif o==0x12: return a, (st[-1] if st else '?')
        elif o==0x26: return a, '0'
        elif o==0x13:
            t=b(a); a+=1
            if t in (1,2,3,4,5,0x50):
                v=i32(a); a+=4; n=b(a); s=r[(a+1)&0x1fffff:(a+1+n)&0x1fffff].split(b'\0')[0].decode('latin1'); a+=1+n
                st.append(f"ref{t}('{s}')")
            elif t in (0x51,0x8e): st.append(f"ref{t}({i32(a)})"); a+=4
            elif t==0: pass
            elif t==9:
                n=b(a); a+=1; parts=[]
                k=1+(1 if n>1 else 0)+(2 if n>3 else 0)
                for _ in range(k):
                    a,e=expr(a); parts.append(e)
                st.append(f"sound9({','.join(parts)})")
            else: st.append(f"?13.{t}"); return a,'??'
        elif o==0x1c: st.append(f"G{u16(a)}"); a+=2
        elif o==0x1d: st.append(f"F{u16(a)}"); a+=2
        elif o==0x2d: st.append(f"parentF{u16(a)}"); a+=2
        elif o==0x23: pass
        elif o in (0x2a,0x2b,0x2c): pass
        else: st.append(f"?e{o:#x}"); return a,'??'
GEN={1: '', 2: '', 3: '', 4: '', 5: '', 6: 'UE', 7: 'UE', 8: 'E', 10: 'E', 11: 'E', 12: '', 13: '', 15: 'E', 18: 'E', 21: 'EU', 22: 'U', 25: '', 29: '', 31: 'E', 32: 'E', 33: 'E', 34: 'E', 35: '', 36: 'E', 37: 'E', 38: 'E', 39: 'E', 40: 'E', 41: 'E', 42: 'E', 43: 'E', 44: 'E', 45: 'E', 46: 'E', 47: 'E', 50: 'U', 54: 'UE', 55: 'UE', 56: 'UE', 58: 'E', 63: 'E', 64: 'UEE', 66: '', 67: 'U', 69: '', 70: '', 71: 'E', 72: '', 73: 'E', 75: '', 76: '', 77: 'EEE', 82: 'E', 83: 'E', 84: '', 85: '', 87: '', 88: 'U', 89: 'U', 90: 'E', 91: 'E', 92: '', 93: '', 94: 'UE', 95: 'E', 96: 'EE', 97: '', 98: '', 101: 'E', 102: 'E', 103: 'UE', 105: '', 106: '', 107: '', 108: 'UE', 109: 'EEEE', 110: 'E', 113: '', 114: '', 115: '', 116: '', 117: '', 118: '', 120: 'E', 125: '', 126: 'EEEE', 127: 'E', 128: '', 129: '', 130: '', 131: '', 135: '', 136: '', 137: 'EE', 138: 'EEEEE', 139: '', 140: '', 141: '', 143: 'EE', 144: 'EEEE', 145: 'E', 146: 'E', 147: 'EE', 149: 'E', 150: 'E', 151: 'E', 152: 'E', 156: '', 161: 'EE', 162: '', 163: '', 164: '', 169: '', 170: '', 171: '', 172: '', 173: '', 174: 'E', 175: '', 178: 'UE', 181: 'I', 184: '', 185: 'E', 186: '', 187: '', 188: '', 190: '', 193: 'EEEEEE', 194: 'E', 195: 'E', 196: 'EE', 197: 'E', 199: '', 202: '', 203: '', 204: '', 205: '', 206: '',
 0x4e:'',0x4f:'',0xbd:'',0xc8:'',0xc9:'',0x9d:'',0xb3:'',0x7a:'E',0x7b:'',0x9e:'',0xa7:'',0xb4:'E',0xc6:'',0x99:'E',0x9a:'E',0x6f:'',0xa8:'',0x63:'',0x64:'',0x7c:'',0xa0:'',0xbf:'',0xc0:'', 0x44:'BU', 0x4a:'B', 0x68:'B'}
REL=BASE
def dis(a):
    o=b(a); p=a+1; ops=[]; nxt=[]
    def E():
        nonlocal p; p,e=expr(p); ops.append(e)
    def U(): 
        nonlocal p; v=u16(p); p+=2; return v
    name=f"op{o:02x}"; term=False
    if o==0x15: E(); off=s16(p); p+=2; ops.append(f"else->{p+off-REL:#x}"); nxt=[p+off]; name='if'
    elif o==0x16: off=s16(p); p+=2; ops.append(f"->{p+off-REL:#x}"); nxt=[p+off]; term=True; name='jmp'
    elif o==0x1b: t=U(); ops.append(f"->{t:#x}"); nxt=[REL+t]; term=True; name='goto'
    elif o==0x1c: t=U(); ops.append(f"sub_{t:#x}"); nxt=[REL+t]; name='call'
    elif o==0x1e: t=U(); ops.append(f"resume {t:#x}"); nxt=[REL+t]; term=True; name='YIELD'
    elif o==0x39: term=True; name='ret'
    elif o==0x1a:
        E(); n=U(); d=U(); ops.append(f"default->{d:#x}"); nxt=[REL+d]
        for _ in range(n):
            t=U(); p,e=expr(p); ops.append(f"{e}->{t:#x}"); nxt.append(REL+t)
        term=True; name='switch'
    elif o==0x36: v=U(); E(); name=f'L{v} ='
    elif o==0x5e: v=U(); E(); name=f'G{v} ='
    elif o==0x6c: v=U(); E(); name=f'F{v} ='
    elif o==0x38: v=U(); E(); name=f'{PROP.get(v,hex(v))} ='
    elif o==0xb0: k=b(p); v=u16(p+1); p+=3; name=f'dec {k}:{v}'
    elif o==0x09:
        n=b(p); p+=1; E()
        if n==2: E()
        if n==4: E();E();E()
        name='sound'
    elif o==0x0b: E(); name='setanim'
    elif o==0x0c: name='animstop'
    elif o==0x11: name='repeat'
    elif o==0x12: E(); name='until(yield)'
    elif o==0xae: E(); name='until(noyield)'
    elif o==0x22: E(); name='JUMP(h)'
    elif o==0x73: name='FALL(v=0x180)'
    elif o==0x76: name='clearjumpfall'
    elif o==0x34: name='pop2ret'
    elif o==0x3c: n=b(p); p+=1; vs=[u16(p+2*i) for i in range(n)]; p+=2*n; ops.append(str(vs)); name='events'
    elif o==0x5f: E(); name='g7527c=(e>>4)'
    elif o==0x66: E(); name='g754cc=(e>>4)'
    elif o==0x65: E(); name='animspeed='
    elif o in (0x6a,0x6b,0x45,0x46,0x71,0x72,0xa0,0xbf,0xc0,0x1d,0x47,0x48):
        name={0x6a:'set174_200000(animspeed by vel)',0x6b:'clr174_200000',0x45:'set174_10000000',0x46:'clr174_10000000',0x71:'set174_2000000(VARJUMP)',0x72:'clr174_2000000',0xa0:'STOPMOVE',0xbf:'MOVE',0xc0:'TURN',0x1d:'restorepos'}.get(o,name)
    elif o==0x10: E(); name='WAIT(ticks)'
    elif o in (0x30,0x77):
        ops.append(f"i{i32(p)}"); p+=4; ops.append(f"u{u16(p)}"); p+=2; ops.append(f"b{b(p)}"); p+=1
        na=b(p+1); ln=b(p+2); p+=3; s=r[p&0x1fffff:(p+ln)&0x1fffff].decode('latin1'); p+=ln; ops.append(repr(s))
        for _ in range(max(0,na-1)): E()
        name='SPAWN' if o==0x30 else 'spawn77'
    elif o==0x70: n=b(p); p+=1; vs=[u16(p+2*i) for i in range(n)]; p+=2*n; ops.append(str(vs)); name='events2'
    elif o==0x13: E(); off=s16(p); p+=2; ops.append(f"else->{p+off-REL:#x}"); nxt=[p+off]; name='while'
    elif o==0x14: term=True; name='endwhile(yield)'
    elif o==0x86: term=True; name='endwhile(noyield)'
    elif o==0x17: off=s16(p); p+=2; ops.append(f"->{p+off-REL:#x}"); nxt=[p+off]; name='ifAnimPlaying_jmp'
    elif o==0x0e: n=b(p); s=r[(p+1)&0x1fffff:(p+1+n)&0x1fffff].decode('latin1'); p+=1+n; name=f'print {s!r}'
    elif o in GEN:
        for c in GEN[o]:
            if c=='E': E()
            elif c=='U': ops.append(f"u{U()}")
            elif c=='I': ops.append(f"i{i32(p)}"); p+=4
            elif c=='B': ops.append(f"b{b(p)}"); p+=1
    else: name=f'UNKNOWN op{o:02x}'; term=True
    return p,name,ops,nxt,term
seen={}; work=[BASE]+[int(x,16) for x in sys.argv[3:]]
unk=set()
while work:
    a=work.pop()
    while a not in seen and BASE<=a<BASE+LEN:
        p,name,ops,nxt,term=dis(a)
        seen[a]=(name,ops,p)
        if name.startswith('UNKNOWN'): unk.add(name)
        work+=nxt
        if term: break
        a=p
for a in sorted(seen):
    n,ops,p=seen[a]; print(f"{a-BASE:04x}: {n} {' | '.join(ops)}")
print('UNK',sorted(unk),file=sys.stderr)
