#!/usr/bin/env python3
"""HW17 electrical prototype sources. Run with KiCad's bundled Python 3.9.

Builds placed boards, local footprint library, native schematics and BOMs.
Routing is a separate reproducible step; never overwrite reviewed routed boards.
"""
import csv, json, math, pathlib, uuid, sys, re
import pcbnew as p
IO = p.PCB_IO_KICAD_SEXPR()

ROOT = pathlib.Path(__file__).resolve().parent
LIB = pathlib.Path('/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints')
BASE = ROOT.parents[1] / 'rev-b/main/main-revb.kicad_pcb'
OUT = ROOT / 'source'
FP = OUT / 'HW17.pretty'
FP.mkdir(parents=True, exist_ok=True)
U = lambda: str(uuid.uuid4())
mm = p.FromMM
V = lambda x,y: p.VECTOR2I(mm(x),mm(y))
Q = lambda x: json.dumps(str(x), ensure_ascii=False)

def layers(ids):
    result=p.LSET()
    for layer in ids:result.AddLayer(layer)
    return result

def pad(fp,n,x,y,w,h,net=None,drill=None,paste=True):
    a=p.PAD(fp); a.SetNumber(str(n)); a.SetPosition(V(x,y)); a.SetSize(V(w,h))
    a.SetShape(p.PAD_SHAPE_ROUNDRECT); a.SetRoundRectRadiusRatio(.15)
    if drill:
        a.SetAttribute(p.PAD_ATTRIB_PTH); a.SetDrillSize(V(drill,drill)); a.SetLayerSet(p.LSET.AllCuMask() | layers([p.F_Mask,p.B_Mask]))
    else:
        a.SetAttribute(p.PAD_ATTRIB_SMD); a.SetLayerSet(layers([p.F_Cu,p.F_Mask,p.F_Paste]) if paste else layers([p.F_Cu,p.F_Mask]))
    a.SetLocalSolderMaskMargin(mm(.025)); fp.Add(a); return a

def line(parent,a,b,layer=p.F_Fab,width=.1):
    o=p.PCB_SHAPE(parent);o.SetShape(p.SHAPE_T_SEGMENT);o.SetStart(V(*a));o.SetEnd(V(*b));o.SetLayer(layer);o.SetWidth(mm(width));parent.Add(o);return o

def rectangle(parent,x0,y0,x1,y1,layer=p.F_Fab,width=.1):
    for a,b in [((x0,y0),(x1,y0)),((x1,y0),(x1,y1)),((x1,y1),(x0,y1)),((x0,y1),(x0,y0))]:line(parent,a,b,layer,width)

def custom(name):
    fp=p.FOOTPRINT(None);fp.SetReference('REF**');fp.SetValue(name);fp.SetAttributes(p.FP_SMD);fp.SetFPID(p.LIB_ID('HW17',name));return fp

def savefp(fp,name):
    fp.Reference().SetVisible(False);fp.Value().SetVisible(False)
    IO.FootprintSave(str(FP),fp)

def footprints():
    b=p.LoadBoard(str(BASE)); f=p.FOOTPRINT(b.FindFootprintByReference('U3'))
    f.SetPosition(V(0,0));f.SetOrientationDegrees(0);f.SetFPID(p.LIB_ID('HW17','FPC31_DIANWEI_8_03A5_031250'))
    for x in f.Pads(): x.SetNetCode(0)
    savefp(f,'FPC31_DIANWEI_8_03A5_031250')
    f=custom('DRV8311_RRW0024A')
    for i in range(7):pad(f,1+i,-1.465,-1.2+.4*i,.47,.2)
    for i in range(5):pad(f,8+i,-.8+.4*i,1.465,.2,.47)
    for i in range(7):pad(f,13+i,1.465,1.2-.4*i,.47,.2)
    for i in range(5):pad(f,20+i,.8-.4*i,-1.465,.2,.47)
    pad(f,25,0,0,1.9,1.9,paste=False)
    for x in [-.475,.475]:
        for y in [-.475,.475]:
            a=pad(f,'',x,y,.75,.75);a.SetLayerSet(layers([p.F_Paste]))
    rectangle(f,-1.5,-1.5,1.5,1.5);rectangle(f,-1.95,-1.95,1.95,1.95,p.F_CrtYd,.05)
    line(f,(-1.75,-1.8),(-1.1,-1.8),p.F_SilkS,.12)
    savefp(f,'DRV8311_RRW0024A')
    f=custom('MT6701QT_QFN16_3x3_P0p5')
    for i in range(4):pad(f,1+i,-1.45,-.75+.5*i,.6,.25)
    for i in range(4):pad(f,5+i,-.75+.5*i,1.45,.25,.6)
    for i in range(4):pad(f,9+i,1.45,.75-.5*i,.6,.25)
    for i in range(4):pad(f,13+i,.75-.5*i,-1.45,.25,.6)
    # Datasheet does not assign an electrical function to the exposed paddle.
    # Solder to an isolated island, not an assumed GND connection.
    pad(f,17,0,0,1.7,1.7,paste=False)
    for x in [-.4,.4]:
        for y in [-.4,.4]:
            a=pad(f,'',x,y,.65,.65);a.SetLayerSet(layers([p.F_Paste]))
    rectangle(f,-1.5,-1.5,1.5,1.5);rectangle(f,-1.85,-1.85,1.85,1.85,p.F_CrtYd,.05)
    savefp(f,'MT6701QT_QFN16_3x3_P0p5')
    f=custom('Flex_Tail_4_SolderPads_P0p35')
    for i in range(4):pad(f,i+1,0,-.525+.35*i,1.5,.25,paste=False)
    rectangle(f,-.85,-.75,.85,.75,p.F_CrtYd,.05)
    savefp(f,'Flex_Tail_4_SolderPads_P0p35')

def component(ref,value,footprint,nets,xy,rot=0,names=None,types=None,mpn='',lcsc='',notes='',sch=None):
    return dict(ref=ref,value=value,footprint=footprint,nets={str(k):v for k,v in nets.items()},xy=xy,rot=rot,
                names={str(k):v for k,v in (names or {}).items()},types={str(k):v for k,v in (types or {}).items()},
                mpn=mpn,lcsc=lcsc,notes=notes,sch=sch,uuid=U())

RFP='Resistor_SMD:R_0603_1608Metric'
CFP='Capacitor_SMD:C_0603_1608Metric'
def hdr(n):return 'Connector_PinHeader_2.54mm:PinHeader_1x%02d_P2.54mm_Vertical'%n

def driver():
    c=[]
    def add(*args,**kw): c.append(component(*args,**kw));return c[-1]
    pin={str(n):None for n in range(1,32)}
    for n in [1,7,12,22,25,28,31]:pin[str(n)]='GND'
    pin.update({'4':'SDA','5':'SCL','8':'HOST_3V3','14':'HOST_EN','16':'HOST_PWM_U','17':'HOST_PWM_V','20':'HOST_PWM_W','21':'nFAULT','MP':'GND'})
    add('J1','DECK DISPLAY FPC 31P','HW17:FPC31_DIANWEI_8_03A5_031250',pin,(6,19),270,
        mpn='8.03A5.031250',lcsc='C55247264',sch=(55,72),notes='31P 0.3mm; 0.2mm FPC; match host pin 1 to adapter pin 1. OLED disconnected.')
    pins={1:'nFAULT',2:'CSAREF',3:'IS_C',4:'IS_B',5:'IS_A',6:'CP',7:'VM',8:'VM',9:'GND',10:'PH_U',11:'PH_V',12:'PH_W',13:'PWM_W',14:'PWM_V',15:'PWM_U',16:'GND',17:'AVDD',18:'AVDD',19:'AVDD',20:'AVDD',21:'AVDD',22:'GND',23:'AVDD',24:'DRV_EN',25:'GND'}
    names={1:'nFAULT',2:'CSAREF',3:'SOC',4:'SOB',5:'SOA',6:'CP',7:'VIN_AVDD',8:'VM',9:'PGND',10:'OUTA',11:'OUTB',12:'OUTC',13:'INHC',14:'INHB',15:'INHA',16:'AGND',17:'AVDD',18:'INLA',19:'INLB',20:'INLC',21:'GAIN',22:'SLEW',23:'MODE',24:'nSLEEP',25:'EP'}
    types={i:'input' for i in range(1,26)}
    for i in [2,7,8,9,16,25]:types[i]='power_in'
    types[17]='power_out';types[1]='open_collector'
    for i in [3,4,5,6,10,11,12]:types[i]='output'
    add('U1','DRV8311HRRWR','HW17:DRV8311_RRW0024A',pins,(29,22),names=names,types=types,mpn='DRV8311HRRWR',lcsc='C5218862',sch=(165,72),notes='3PWM; GAIN=2V/A; SLEW=35V/us; OCP=5A (not motor-safe current regulation).')
    add('J2','5V INPUT',hdr(2),{1:'VIN_5V',2:'GND'},(44,7.3),0,names={1:'+5V',2:'GND'},sch=(263,34),notes='External current-limited bench supply only; no connection to host power rails.')
    add('J3','ENCODER',hdr(4),{1:'GND',2:'HOST_3V3',3:'SDA',4:'SCL'},(9,7),90,sch=(263,78))
    add('J4','MOTOR U V W',hdr(3),{1:'PH_U',2:'PH_V',3:'PH_W'},(44,26),0,sch=(354,34))
    add('J5','ARM - SHUNT REMOVED',hdr(2),{1:'ARM_IN',2:'DRV_EN'},(15,32),90,sch=(354,65),notes='Fit removable shunt only after bench firmware initializes. No shunt in shipped assembly.')
    add('J6','CURRENT MONITOR',hdr(5),{1:'IS_A_MON',2:'IS_B_MON',3:'IS_C_MON',4:'CSAREF',5:'GND'},(21,4.5),90,sch=(354,95),notes='For oscilloscope/external ADC; NOT connected to SF32 ADC.')
    add('D1','SS14','Diode_SMD:D_SMA',{1:'VM',2:'VIN_5V'},(37,8),180,names={1:'K',2:'A'},mpn='SS14',lcsc='C2480')
    add('D2','SMAJ5.0A','Diode_SMD:D_SMA',{1:'VM',2:'GND'},(45,17),90,names={1:'K',2:'A'},mpn='SMAJ5.0A',notes='Unidirectional 5V-standoff TVS; limits bus transients, not a sustained braking load.')
    def r(ref,val,n1,n2,xy,rot=0,notes=''):
        return add(ref,val,RFP,{1:n1,2:n2},xy,rot,mpn='' if val=='0R' else 'RC0603FR-07'+{'100R':'100RL','1k':'1KL','10k':'10KL','47k':'47KL','100k':'100KL','4.7k':'4K7L','330R':'330RL'}[val],notes=notes)
    r('R1','1k','HOST_EN','ARM_IN',(11,27),90)
    r('R2','47k','DRV_EN','GND',(23,30),90)
    for i,(ph,yy) in enumerate([('U',19),('V',22),('W',25)]):
        r('R'+str(3+i),'100R','HOST_PWM_'+ph,'PWM_'+ph,(37,yy))
        r('R'+str(6+i),'100k','PWM_'+ph,'GND',(33.5,yy),90)
    r('R9','10k','HOST_3V3','nFAULT',(17.5,16),90)
    r('R10','4.7k','HOST_3V3','SDA',(13,12),90)
    r('R11','4.7k','HOST_3V3','SCL',(16,12),90)
    r('R12','0R','AVDD','CSAREF',(25,16),0)
    for i,ph in enumerate('ABC'):r('R'+str(13+i),'330R','IS_'+ph,'IS_'+ph+'_MON',(21+i*3,11),90)
    caps=[('C1','100nF 16V','VIN_5V','GND',(47.5,10),90,CFP),
          ('C2','100uF 16V','VM','GND',(36,13.5),0,'Capacitor_THT:CP_Radial_D6.3mm_P2.50mm'),
          ('C3','22uF 16V','VM','GND',(21,27),180,'Capacitor_SMD:C_1206_3216Metric'),
          ('C4','22uF 16V','VM','GND',(30,30),0,'Capacitor_SMD:C_1206_3216Metric'),
          ('C5','100nF 16V','VM','GND',(28.2,26),270,CFP),
          ('C6','100nF 16V','VM','GND',(25,24.3),0,CFP),
          ('C7','100nF 16V','CP','VM',(25,22),180,CFP),
          ('C8','2.2uF 10V','AVDD','GND',(31.2,18),0,CFP),
          ('C9','100nF 10V','CSAREF','GND',(25,18.5),0,CFP),
          ('C10','1uF 10V','HOST_3V3','GND',(10,12),90,CFP)]
    for ref,val,a,b,xy,rot,fp in caps:add(ref,val,fp,{1:a,2:b},xy,rot,notes='C2 pin 1 positive.' if ref=='C2' else '')
    for i,ph in enumerate('ABC'):add('C'+str(11+i),'22pF C0G',CFP,{1:'IS_'+ph+'_MON',2:'GND'},(21+i*3,14),0)
    # High-frequency bypasses are routed manually before the remaining nets.
    return dict(name='HW17-driver-r0',title='HW17 / Rev A-B FPC motor driver - BENCH PROTOTYPE',parts=c,thickness=1.0,kind='driver')

def sensor():
    nets={i:None for i in range(1,18)}
    nets.update({6:'SDA',7:'SCL',8:'VDD',13:'VDD',14:'VDD',16:'GND'})
    names={1:'NC',2:'NC',3:'NC',4:'NC',5:'PUSH',6:'SDA/A',7:'SCL/B',8:'CSN/Z',9:'W',10:'NC',11:'U',12:'V',13:'VDD',14:'MODE',15:'OUT',16:'GND',17:'EP_ISOLATED'}
    types={6:'bidirectional',7:'input',8:'input',13:'power_in',14:'input',16:'power_in'}
    c=[component('U1','MT6701QT-STD','HW17:MT6701QT_QFN16_3x3_P0p5',nets,(0,0),names=names,types=types,
                 mpn='MT6701QT-STD',lcsc='C2913974',sch=(105,65),notes='MODE and CSN tied VDD for I2C. EP soldered to isolated island; confirm EP policy with manufacturer.'),
       component('C1','100nF 10V','Capacitor_SMD:C_0201_0603Metric',{1:'VDD',2:'GND'},(0,-2.25),sch=(200,85)),
       component('J1','WIRE TAIL','HW17:Flex_Tail_4_SolderPads_P0p35',{1:'GND',2:'SDA',3:'SCL',4:'VDD'},(-7.071068,-7.071068),135,sch=(200,45),notes='Four solder lands at 10mm radius; solder only beyond motor perimeter; strain-relieve wires.')]
    return dict(name='HW17-encoder-r0',title='HW17 / MT6701 magnetic encoder flex - BENCH PROTOTYPE',parts=c,thickness=.15,kind='sensor')

def board(spec):
    folder=OUT/spec['name'];folder.mkdir(exist_ok=True)
    b=p.BOARD();b.GetDesignSettings().SetBoardThickness(mm(spec['thickness']))
    b.GetDesignSettings().m_MinClearance=mm(.12 if spec['kind']=='driver' else .10)
    b.GetDesignSettings().m_TrackMinWidth=mm(.12)
    b.GetDesignSettings().m_ViasMinSize=mm(.4);b.GetDesignSettings().m_MinThroughDrill=mm(.2)
    # Default routing widths; power paths get their own rule below.
    netnames=sorted({n for c in spec['parts'] for n in c['nets'].values() if n})
    nets={}
    for n in netnames:
        ni=p.NETINFO_ITEM(b,n);b.Add(ni);nets[n]=ni
    offset=(100,100)
    for c in spec['parts']:
        lib,fn=c['footprint'].split(':')
        f=IO.FootprintLoad(str(FP if lib=='HW17' else LIB/(lib+'.pretty')),fn)
        if not f:raise ValueError(c['footprint'])
        f.SetReference(c['ref']);f.SetValue(c['value']);f.SetFPID(p.LIB_ID(lib,fn))
        f.SetPath(p.KIID_PATH('/'+spec['root_uuid']+'/'+c['uuid']))
        f.SetPosition(V(c['xy'][0]+100,c['xy'][1]+100));f.SetOrientationDegrees(c['rot'])
        f.Reference().SetVisible(spec['kind']=='driver');f.Reference().SetTextSize(V(.8,.8));f.Reference().SetTextThickness(mm(.12));f.Reference().SetLayer(p.F_Fab)
        f.Value().SetVisible(False)
        if spec['kind']=='sensor': f.Reference().SetVisible(False)
        for a in f.Pads():
            name=c['nets'].get(a.GetNumber())
            a.SetNet(nets[name] if name else b.FindNet(0))
        b.Add(f)
    def bl(a,z):line(b,(a[0]+100,a[1]+100),(z[0]+100,z[1]+100),p.Edge_Cuts,.05)
    if spec['kind']=='driver':
        rectangle(b,100,100,150,140,p.Edge_Cuts,.05)
        for x,y in [(3,3),(47,3),(3,37),(47,37)]:
            f=IO.FootprintLoad(str(LIB/'MountingHole.pretty'),'MountingHole_2.2mm_M2');f.SetPosition(V(x+100,y+100));f.SetReference('H'+str(int(x+y)));f.Reference().SetVisible(False);f.Value().SetVisible(False);f.SetAttributes(p.FP_EXCLUDE_FROM_BOM | p.FP_EXCLUDE_FROM_POS_FILES | p.FP_BOARD_ONLY);b.Add(f)
        for text,x,y,size in [('HW17 DRIVER R0',20,37,1.2),('5V EXT',39,3,1),('NO OLED',7,30,.8),('ARM',15,35,.8),('U V W',41,36,.8),('G 3V3 D C',12,4,.8)]:
            t=p.PCB_TEXT(b);t.SetText(text);t.SetPosition(V(x+100,y+100));t.SetTextSize(V(size,size));t.SetTextThickness(mm(.12));t.SetLayer(p.F_SilkS);b.Add(t)
        # Thermal vias on centre lines outside the four paste apertures.
        for dx,dy in [(0,0),(.7,0),(-.7,0),(0,.7),(0,-.7)]:
            v=p.PCB_VIA(b);v.SetPosition(V(129+dx,122+dy));v.SetWidth(mm(.4));v.SetDrill(mm(.2));v.SetViaType(p.VIATYPE_THROUGH);v.SetLayerPair(p.F_Cu,p.B_Cu);v.SetNet(nets['GND']);b.Add(v)
    else:
        # Circle with radial 1.6mm tail, oriented at CAD angle +135deg.
        # KiCad Y increases down: CAD +Y is -Y here.
        r=2.75;half=.8;ang=math.radians(-135);delta=math.asin(half/r)
        pts=[]
        for i in range(121):
            a=ang+delta+(2*math.pi-2*delta)*i/120
            pts.append((r*math.cos(a),r*math.sin(a)))
        d=(math.cos(ang),math.sin(ang));normal=(-d[1],d[0])
        # Last point is lower side in angular coordinates; extend along it.
        pts += [(11*d[0]-half*normal[0],11*d[1]-half*normal[1]),(11*d[0]+half*normal[0],11*d[1]+half*normal[1])]
        for a,z in zip(pts,pts[1:]+pts[:1]):bl(a,z)
        o=p.PCB_SHAPE(b);o.SetShape(p.SHAPE_T_CIRCLE);o.SetCenter(V(100,100));o.SetEnd(V(102.75,100));o.SetLayer(p.User_1);o.SetWidth(mm(.05));b.Add(o)
        t=p.PCB_TEXT(b);t.SetText('PI STIFFENER D5.5 / TOTAL 0.40mm');t.SetPosition(V(100,105));t.SetTextSize(V(.65,.65));t.SetTextThickness(mm(.1));t.SetLayer(p.User_1);b.Add(t)
    proj={'meta':{'filename':spec['name']+'.kicad_pro','version':1},'board':{'design_settings':{'rules':{'min_clearance':.12 if spec['kind']=='driver' else .1,'min_track_width':.12,'min_via_diameter':.4,'min_through_hole_diameter':.2,'min_hole_to_hole':.2,'min_copper_edge_clearance':.2 if spec['kind']=='driver' else .1,'solder_mask_to_copper_clearance':0.0},'rule_severities':{'silk_over_copper':'warning','courtyards_overlap':'error','missing_courtyard':'warning','lib_footprint_mismatch':'warning'}}},'net_settings':{'classes':[{'name':'Default','clearance':.12 if spec['kind']=='driver' else .1,'track_width':.15 if spec['kind']=='driver' else .12,'via_diameter':.5 if spec['kind']=='driver' else .4,'via_drill':.25 if spec['kind']=='driver' else .2,'microvia_diameter':.3,'microvia_drill':.1,'wire_width':6,'bus_width':12}],'meta':{'version':4},'netclass_assignments':{}}}
    (folder/(spec['name']+'.kicad_pro')).write_text(json.dumps(proj,indent=2))
    (folder/(spec['name']+'-placed.kicad_pro')).write_text(json.dumps(proj,indent=2))
    (folder/'fp-lib-table').write_text('(fp_lib_table\n (version 7)\n (lib (name "HW17")(type "KiCad")(uri "${KIPRJMOD}/../HW17.pretty")(options "")(descr "HW17 verified local footprints")))\n')
    path=folder/(spec['name']+'-placed.kicad_pcb');p.SaveBoard(str(path),b)
    (folder/(spec['name']+'-placed.kicad_pro')).write_text(json.dumps(proj,indent=2))
    return b

def schematic(spec):
    folder=OUT/spec['name'];root=spec['root_uuid'];syms=[];items=[]
    def text(s,x,y,size=1.27):return f'(text {Q(s)} (at {x} {y} 0)(effects(font(size {size} {size}))(justify left))(uuid {U()}))'
    passive_index=0
    for c in spec['parts']:
        ref=c['ref'];inbom='no' if spec['kind']=='sensor' and ref=='J1' else 'yes';pins=list(c['nets']);pins.sort(key=lambda z:int(z) if z.isdigit() else 999)
        sid=ref;nn=len(pins);small=nn==2 and ref[0] in 'RCD'
        if small:
            pos=[(-5.08,0,0),(5.08,0,180)];bw=2.54;bh=2.54
        else:
            half=(nn+1)//2;bw=17.78;bh=max(7.62,(half+1)*2.54/2)
            pos=[]
            for i in range(nn):
                if i<half:pos.append((-bw-5.08,((half-1)//2)*2.54-i*2.54,0))
                else:pos.append((bw+5.08,((half-1)//2)*2.54-(i-half)*2.54,180))
        definition=f'(symbol {Q("HW17:"+sid)} (pin_names(offset 1.016)) (in_bom {inbom})(on_board yes) (property "Reference" {Q(ref[0])}(at 0 {bh+2.54} 0)(effects(font(size 1.27 1.27)))) (property "Value" {Q(c["value"])}(at 0 {-bh-2.54} 0)(effects(font(size 1.27 1.27))))'
        if small and ref.startswith('C'):
            drawing='(polyline(pts(xy -0.635 2.54)(xy -0.635 -2.54))(stroke(width .254)(type default))(fill(type none)))'+'(polyline(pts(xy 0.635 2.54)(xy 0.635 -2.54))(stroke(width .254)(type default))(fill(type none)))'
        elif small and ref.startswith('D'):
            drawing='(polyline(pts(xy -1.27 2.54)(xy -1.27 -2.54))(stroke(width .254)(type default))(fill(type none)))'+'(polyline(pts(xy -1.27 0)(xy 1.27 2.54)(xy 1.27 -2.54)(xy -1.27 0))(stroke(width .254)(type default))(fill(type none)))'
        else:
            drawing=f'(rectangle(start {-bw} {1.016 if small else bh})(end {bw} {-1.016 if small else -bh})(stroke(width .254)(type default))(fill(type background)))'
        definition+=f'(symbol {Q(sid+"_0_1")} {drawing})'
        definition+=f'(symbol {Q(sid+"_1_1")}'
        for pn,(x,y,ang) in zip(pins,pos):
            typ=c['types'].get(pn,'passive');nm='~' if small else c['names'].get(pn,pn)
            definition+=f'(pin {typ} line(at {x} {y} {ang})(length {(4.445 if ref.startswith("C") else 3.81 if ref.startswith("D") else 2.54) if small else 5.08})(name {Q(nm)}(effects(font(size 1.016 1.016))))(number {Q(pn)}(effects(font(size 1.016 1.016)))))'
        definition+='))';syms.append(definition)
        if c['sch']:cx,cy=c['sch']
        else:
            cx=45+(passive_index%5)*76.2;cy=142.24+(passive_index//5)*20.32;passive_index+=1
        cx=round(cx/1.27)*1.27;cy=round(cy/1.27)*1.27
        props=''
        for name,val,py,hide in [('Reference',ref,cy-bh-5.08,False),('Value',c['value'],cy-bh-2.54,False),('Footprint',c['footprint'],cy,True),('Datasheet','',cy,True)]:
            props+=f'(property {Q(name)} {Q(val)}(at {cx} {py} 0)(effects(font(size 1.27 1.27))'+(' (hide yes)' if hide else '')+'))'
        instance=f'(symbol(lib_id {Q("HW17:"+sid)})(at {cx} {cy} 0)(unit 1)(in_bom {inbom})(on_board yes)(dnp no)(uuid {c["uuid"]}){props}'
        for pn in pins:instance+=f'(pin {Q(pn)}(uuid {U()}))'
        instance+=f'(instances(project {Q(spec["name"])}(path {Q("/"+root)}(reference {Q(ref)})(unit 1)))))'
        items.append(instance)
        for pn,(x,y,ang) in zip(pins,pos):
            xx,yy=cx+x,cy-y;net=c['nets'][pn]
            if net is None:items.append(f'(no_connect(at {xx} {yy})(uuid {U()}))');continue
            end=xx+(-7.62 if x<0 else 7.62)
            items.append(f'(wire(pts(xy {xx} {yy})(xy {end} {yy}))(stroke(width 0)(type default))(uuid {U()}))')
            items.append(f'(label {Q(net)}(at {end} {yy} {0 if x>0 else 180})(effects(font(size 1.016 1.016))(justify left bottom))(uuid {U()}))')
    # Explicit power sources at the carrier connectors / sensor harness.
    powered=['HOST_3V3','GND','VIN_5V','VM','CSAREF'] if spec['kind']=='driver' else ['VDD','GND']
    for i,net in enumerate(powered):
        sid='PWR'+str(i);cx=30+i*40;cy=118 if spec['kind']=='driver' else 115
        cx=round(cx/1.27)*1.27;cy=round(cy/1.27)*1.27
        syms.append(f'(symbol {Q("HW17:"+sid)}(power)(pin_names(offset 0))(in_bom no)(on_board no)(property "Reference" "#FLG"(at 0 0 0)(effects(font(size 1.27 1.27))(hide yes)))(property "Value" "PWR_FLAG"(at 0 2.54 0)(effects(font(size 1.27 1.27))(hide yes)))(symbol {Q(sid+"_1_1")}(pin power_out line(at 0 0 90)(length 0)(name "pwr"(effects(font(size 1.27 1.27))))(number "1"(effects(font(size 1.27 1.27)))))))')
        items.append(f'(symbol(lib_id {Q("HW17:"+sid)})(at {cx} {cy} 0)(unit 1)(in_bom no)(on_board no)(uuid {U()})(property "Reference" "#FLG0{i}"(at {cx} {cy} 0)(effects(font(size 1.27 1.27))(hide yes)))(property "Value" "PWR_FLAG"(at {cx} {cy} 0)(effects(font(size 1.27 1.27))(hide yes)))(instances(project {Q(spec["name"])}(path {Q("/"+root)}(reference "#FLG0{i}")(unit 1)))))')
        items.append(f'(label {Q(net)}(at {cx} {cy} 0)(effects(font(size 1.016 1.016))(justify left bottom))(uuid {U()}))')
    if spec['kind']=='driver':
        items.append(text('HOST FPC / OLED DISCONNECTED',20,18,1.8));items.append(text('DRV8311H / 3-PWM',130,18,1.8));items.append(text('EXTERNAL POWER / HARNESS',235,18,1.8))
        items.append(text('R0 bench prototype. Motor phase current is NOT regulated by this circuit. IC OCP=5A protects the driver, not the 0.13mm winding.',20,275,1.05))
        items.append(text('Use current-limited 5V supply; conservative voltage-command limit; scope SOA/B/C. Arm only with dedicated firmware. Host 3V3 and AVDD are separate.',20,279,1.05))
    else:
        items.append(text('MT6701 / 14-bit I2C encoder',30,20,1.8));items.append(text('MODE and CSN tied high. I2C pull-ups are on driver board. Address 0x06; angle registers 0x03 / 0x04.',25,140))
        items.append(text('2-layer flex 0.15mm + central PI stiffener 0.25mm incl. adhesive = 0.40mm island. Island D5.5; tail width 1.6mm.',25,147))
        items.append(text('Chip top faces D4x0.5 diametrical magnet. Nominal top clearance 0.63mm. Magnetic field / interference not yet validated.',25,154))
        items.append(text('EP: isolated solder island; datasheet gives no electrical assignment. Confirm with MagnTek before production.',25,161))
    page='A3' if spec['kind']=='driver' else 'A4'
    s=f'(kicad_sch(version 20250114)(generator "eeschema")(uuid {root})(paper "{page}")(title_block(title {Q(spec["title"])})(date "2026-10-30")(rev "R0")(company "OpenWalkieTalkie"))(lib_symbols'+''.join(syms)+')'+''.join(items)+')\n'
    (folder/(spec['name']+'.kicad_sch')).write_text(s)
    (folder/'sym-lib-table').write_text('(sym_lib_table\n (version 7)\n (lib (name "HW17") (type "KiCad") (uri "${KIPRJMOD}/HW17.kicad_sym") (options "") (descr "HW17 symbols"))\n)\n')
    # External library contains unprefixed symbols; schematic embeds its own copy.
    lib='(kicad_symbol_lib(version 20241209)(generator "kicad_symbol_editor")'+''.join(x.replace('"HW17:','"',1) for x in syms)+')'
    (folder/'HW17.kicad_sym').write_text(lib)
    with (folder/'bom.csv').open('w') as f:
        w=csv.writer(f);w.writerow(['Reference','Value','Footprint','MPN','LCSC','Quantity','Notes'])
        for c in spec['parts']:
            if spec['kind']=='sensor' and c['ref']=='J1':continue
            w.writerow([c['ref'],c['value'],c['footprint'],c['mpn'],c['lcsc'],1,c['notes']])

def main():
    footprints()
    specs=[driver(),sensor()]
    for spec in specs:
        spec['root_uuid']=U();board(spec);schematic(spec)
        (OUT/spec['name']/'circuit.json').write_text(json.dumps(spec,indent=2)+'\n')
        print(spec['name'],len(spec['parts']),'parts')

if __name__=='__main__':main()
