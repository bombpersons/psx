# Writes psx_gte.sinc: python3 gte.py psx_gte.sinc
# The GTE (coprocessor 2) registers and commands by name, for psx.slaspec.
import sys
data="VXY0 VZ0 VXY1 VZ1 VXY2 VZ2 RGBC OTZ IR0 IR1 IR2 IR3 SXY0 SXY1 SXY2 SXYP SZ0 SZ1 SZ2 SZ3 RGB0 RGB1 RGB2 RES1 MAC0 MAC1 MAC2 MAC3 IRGB ORGB LZCS LZCR".split()
ctrl="RT11RT12 RT13RT21 RT22RT23 RT31RT32 RT33 TRX TRY TRZ L11L12 L13L21 L22L23 L31L32 L33 RBK GBK BBK LR1LR2 LR3LG1 LG2LG3 LB1LB2 LB3 RFC GFC BFC OFX OFY H DQA DQB ZSF3 ZSF4 FLAG".split()
cmds={0x01:"RTPS",0x06:"NCLIP",0x0C:"OP",0x10:"DPCS",0x11:"INTPL",0x12:"MVMVA",0x13:"NCDS",0x14:"CDP",0x16:"NCDT",0x1B:"NCCS",0x1C:"CC",0x1E:"NCS",0x20:"NCT",0x28:"SQR",0x29:"DCPL",0x2A:"DPCT",0x2D:"AVSZ3",0x2E:"AVSZ4",0x30:"RTPT",0x3D:"GPF",0x3E:"GPL",0x3F:"NCCT"}
noflags={"NCLIP","AVSZ3","AVSZ4"}
o=[]
o.append("# The PlayStation's GTE (coprocessor 2) by name, and GCC's division checks folded.")
for n in data: o.append(f"define pcodeop gte_get_{n}; define pcodeop gte_set_{n};")
for n in ctrl: o.append(f"define pcodeop gte_get_{n}; define pcodeop gte_set_{n};")
for n in cmds.values(): o.append(f"define pcodeop gte_{n};")
for i,n in enumerate(data):
    o.append(f":mtc2 RTsrc, \"{n}\" is $(AMODE) & prime=0x12 & copop=4 & rd={i} & RTsrc {{ gte_set_{n}(RTsrc); }}")
    o.append(f":mfc2 RT, \"{n}\" is $(AMODE) & prime=0x12 & copop=0 & rd={i} & RT {{ tmp:4 = gte_get_{n}(); RT = sext(tmp); }}")
    o.append(f":lwc2 \"{n}\", OFF_BASE is $(AMODE) & REL6=0 & prime=0x32 & rt={i} & OFF_BASE {{ gte_set_{n}(*[ram]:4 OFF_BASE); }}")
    o.append(f":swc2 \"{n}\", OFF_BASE is $(AMODE) & REL6=0 & prime=0x3A & rt={i} & OFF_BASE {{ tmp:4 = gte_get_{n}(); *[ram]:4 OFF_BASE = tmp; }}")
for i,n in enumerate(ctrl):
    o.append(f":ctc2 RTsrc, \"{n}\" is $(AMODE) & prime=0x12 & copop=6 & rd={i} & RTsrc {{ gte_set_{n}(RTsrc); }}")
    o.append(f":cfc2 RT, \"{n}\" is $(AMODE) & prime=0x12 & copop=2 & rd={i} & RT {{ tmp:4 = gte_get_{n}(); RT = sext(tmp); }}")
for f,n in cmds.items():
    if n=="MVMVA":
        o.append(f":{n.lower()} sf, mx, v, cv, lm is $(AMODE) & prime=0x12 & bit25=1 & fct={f} & cofun [ sf = (cofun >> 19) & 1; mx = (cofun >> 17) & 3; v = (cofun >> 15) & 3; cv = (cofun >> 13) & 3; lm = (cofun >> 10) & 1; ] {{ gte_{n}(sf:1, mx:1, v:1, cv:1, lm:1); }}")
    elif n in noflags:
        o.append(f":{n.lower()} is $(AMODE) & prime=0x12 & bit25=1 & fct={f} & cofun {{ gte_{n}(); }}")
    else:
        o.append(f":{n.lower()} sf, lm is $(AMODE) & prime=0x12 & bit25=1 & fct={f} & cofun [ sf = (cofun >> 19) & 1; lm = (cofun >> 10) & 1; ] {{ gte_{n}(sf:1, lm:1); }}")
# GCC's div macro: break 7 (divide by zero) and break 6 (overflow) after the checks.
o.append(":break \"7\" is $(AMODE) & prime=0 & fct=0xD & breakcode=0x1C00 { }")
o.append(":break \"6\" is $(AMODE) & prime=0 & fct=0xD & breakcode=0x1800 { }")
open(sys.argv[1],"w").write("\n".join(o)+"\n")
