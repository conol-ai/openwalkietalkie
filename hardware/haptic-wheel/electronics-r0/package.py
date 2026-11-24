"""Export the reviewed master boards and assemble a portable review/fabrication ZIP."""
from pathlib import Path
import subprocess,shutil,json,hashlib,zipfile
from PIL import Image,ImageChops
from reportlab.pdfgen import canvas
from reportlab.lib.colors import HexColor
from reportlab.lib.pagesizes import A4
from pypdf import PdfReader,PdfWriter

ROOT=Path(__file__).resolve().parent;REPO=ROOT.parents[2]
PDF=REPO/'output/pdf';WORK=REPO/'target/motor-electronics'
CLI='/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli'
def run(*args):subprocess.run([CLI,*map(str,args)],check=True,stdout=subprocess.DEVNULL)
for kind in ['driver','encoder']:
 name='HW17-'+kind+'-r0';src=ROOT/'source'/name;pcb=src/(name+'.kicad_pcb');fab=ROOT/'fabrication'/kind
 report=json.loads((ROOT/f'review/{kind}-drc.json').read_text())
 assert not report['violations'] and not report['unconnected_items'] and not report['schematic_parity']
 erc=json.loads((ROOT/f'review/{kind}-erc.json').read_text());assert all(not sh['violations'] for sh in erc['sheets'])
 run('pcb','export','gerbers','-l','F.Cu,B.Cu,F.Mask,B.Mask,F.Paste,Edge.Cuts,'+('F.SilkS' if kind=='driver' else 'User.1'),'-o',str(fab)+'/',pcb)
 run('pcb','export','drill','--excellon-separate-th','--generate-report','-o',str(fab)+'/',pcb)
 run('pcb','export','pos','--format','csv','--units','mm','--smd-only','-o',fab/'placement.csv',pcb)
 shutil.copy2(src/'bom.csv',fab/'bom.csv')
 run('sch','export','pdf','-o',PDF/(name+'-schematic.pdf'),src/(name+'.kicad_sch'))
 run('pcb','export','pdf','-l','F.Cu,Edge.Cuts,'+('F.SilkS' if kind=='driver' else 'B.Cu'),'--mode-single','--scale','0','-o',PDF/(name+'-layout.pdf'),pcb)
 run('pcb','export','svg','-l','F.Cu,Edge.Cuts,'+('F.SilkS' if kind=='driver' else 'B.Cu'),'--mode-single','--fit-page-to-board','--exclude-drawing-sheet','-o',ROOT/'review'/(kind+'-layout.svg'),pcb)
 subprocess.run(['pdftoppm','-scale-to','1400','-png','-singlefile',str(PDF/(name+'-layout.pdf')),str(WORK/(kind+'-layout-final'))],check=True)
 im=Image.open(WORK/(kind+'-layout-final.png')).convert('RGB');mask=ImageChops.difference(im,Image.new('RGB',im.size,'white')).convert('L').point(lambda v:255 if v>25 else 0)
 im.crop(mask.getbbox()).save(ROOT/'review'/(kind+'-preview.png'))

W,H=A4;ink=HexColor('#152b3b');muted=HexColor('#526777');accent=HexColor('#007e87')
c=canvas.Canvas(str(WORK/'overview.pdf'),pagesize=A4)
def text(x,y,s,size=10,bold=False,color=ink):
 c.setFillColor(color);c.setFont('Helvetica-Bold' if bold else 'Helvetica',size);c.drawString(x,y,s)
def lines(x,y,ss,size=10,leading=16):
 for s in ss:text(x,y,s,size);y-=leading
 return y
def head(n,title):
 c.setFillColor(accent);c.rect(36,H-48,32,4,fill=1,stroke=0)
 text(36,H-82,title,22,True);text(36,H-104,'OpenWalkieTalkie  /  Engineering prototype  /  30 October 2026',9,color=muted)
 text(36,30,'HW17 ELECTRONICS R0  |  Bench prototype; physical validation pending',8,color=muted);text(W-50,30,str(n),8)
def table(x,y,width,heads,rows,colwidths,rowh=22):
 c.setFillColor(ink);c.rect(x,y-rowh,width,rowh,fill=1,stroke=0)
 xx=x
 for h,w in zip(heads,colwidths):text(xx+7,y-rowh+7,h,9,True,HexColor('#ffffff'));xx+=w
 y-=rowh
 for i,row in enumerate(rows):
  c.setFillColor(HexColor('#edf3f5') if i%2==0 else HexColor('#fafbfc'));c.rect(x,y-rowh,width,rowh,fill=1,stroke=0)
  xx=x
  for v,w in zip(row,colwidths):text(xx+7,y-rowh+7,str(v),9);xx+=w
  y-=rowh
 return y
head(1,'Motor driver + magnetic encoder')
lines(36,H-137,['Two boards for our custom 12-slot / 14-pole wheel. The carrier uses the deck\'s',
 'Rev A r6 / Rev B display FPC. The small encoder sits under the shaft magnet.'])
text(36,H-198,'DRIVER CARRIER',11,True,accent);text(315,H-198,'ENCODER FLEX',11,True,accent)
c.drawImage(str(ROOT/'review/driver-preview.png'),36,H-405,width=245,height=195,preserveAspectRatio=True,anchor='c',mask='auto')
c.drawImage(str(ROOT/'review/encoder-preview.png'),315,H-405,width=235,height=195,preserveAspectRatio=True,anchor='c',mask='auto')
lines(36,H-426,['50 x 40 mm, 1.0 mm FR4, 2 layers','DRV8311H, external 5 V, 3-PWM','Removable ARM jumper; fault input'],10)
lines(315,H-426,['5.5 mm island, 1.6 mm tail','MT6701QT-STD, 14-bit I2C','0.40 mm maximum stiffened stack'],10)
text(36,H-505,'VERIFIED IN THE DESIGN FILES',11,True,accent)
lines(36,H-529,['Both schematics: 0 ERC violations. Both boards: 0 DRC violations,',
 '0 unconnected nets and 0 schematic-to-PCB parity issues.',
 'Encoder STEP: valid solids; zero nominal interference with the checked carriers.',
 'Nominal maximum chip-top to shaft-magnet gap: 0.63 mm.'])
text(36,H-616,'BEFORE POWERING',11,True,accent)
lines(36,H-640,['OLED must be unplugged. Dedicated motor-control firmware is still required.',
 'Use external current-limited 5 V; leave ARM open until firmware is initialized.',
 'No phase-current ADC is available on these FPCs: R0 uses voltage-mode control.',
 'Read README.md and the Chinese fabrication notes before ordering or assembly.',
 'Magnetic accuracy, current limits and temperature rise require hardware testing.'],10)
c.showPage();head(2,'Wiring and assembly reference')
y=table(36,H-130,W-72,['Driver J1 / host FPC','Host signal','Motor-board function'],[
 ('4','PA41','SDA'),('5','PA42','SCL'),('8','IO_3V3','Encoder / logic supply'),('14','PA9','Enable via ARM jumper'),
 ('16','PA5','PWM U'),('17','PA6','PWM V'),('20','PA7','PWM W'),('21','PA2','nFAULT (active low)'),
 ('1,7,12,22,25,28,31','GND','Common ground')],[165,115,W-352])
text(36,y-26,'SENSOR HARNESS: NOT A STRAIGHT-THROUGH PIN MAP',10,True,accent)
y=table(36,y-38,W-72,['Signal','Carrier J3','Encoder flex J1'],[('GND','1','1'),('3V3 / VDD','2','4'),('SDA','3','2'),('SCL','4','3')],[165,115,W-352])
y=lines(36,y-25,['J2: 1 = +5 V, 2 = GND.  J4: 1 = U, 2 = V, 3 = W.',
 'J6: 1 = current A, 2 = B, 3 = C, 4 = CSAREF, 5 = GND.',
 'J5: leave the shunt removed until ready to arm. C2 pin 1 is positive.',
 'Continuity-check the 31-pin cable; verify its contact orientation before insertion.'],10)
text(36,y-17,'ENCODER BUILD DETAILS',11,True,accent)
lines(36,y-40,['0.15 mm flex + nominal 0.25 mm PI/adhesive stiffener; island total <=0.40 mm.',
 'Use 0201 / metric 0603, 100 nF capacitor; not a 0402 substitution.',
 'All vias are inside the stiffened circle. Confirm via filling / solder-wicking control.',
 'The exposed MT6701 paddle is isolated; confirm treatment with its manufacturer.',
 'Original tail angle: 135 deg. For the HW18-B1 120 deg channel, rotate -15 deg.',
 'STEP envelopes are mechanical clearance models, not detailed package models.'],9.5,15)
c.save()
writer=PdfWriter()
for file in [WORK/'overview.pdf',PDF/'HW17-driver-r0-schematic.pdf',PDF/'HW17-encoder-r0-schematic.pdf',PDF/'HW17-driver-r0-assembly.pdf',PDF/'HW17-driver-r0-layout.pdf',PDF/'HW17-encoder-r0-layout.pdf']:
 writer.append(str(file))
writer.write(str(PDF/'HW17-electronics-r0-review.pdf'))
for file in PDF.glob('HW17-*-r0-*.pdf'):shutil.copy2(file,ROOT/'review'/file.name)
shutil.copy2(PDF/'HW17-electronics-r0-review.pdf',ROOT/'review/HW17-electronics-r0-review.pdf')
for kind in ['driver','encoder']:
 with zipfile.ZipFile(ROOT/'fabrication'/('HW17-'+kind+'-r0-fabrication.zip'),'w',zipfile.ZIP_DEFLATED) as z:
  for f in sorted((ROOT/'fabrication'/kind).iterdir()):z.write(f,f.name)
files=[]
for f in sorted(ROOT.rglob('*')):
 if not f.is_file() or '__pycache__' in f.parts or f.name=='SHA256SUMS.txt':continue
 if any(x in f.name for x in ['-placed.','-pre.','.kicad_prl','~','.bak']):continue
 files.append(f)
(ROOT/'SHA256SUMS.txt').write_text(''.join(hashlib.sha256(f.read_bytes()).hexdigest()+'  '+str(f.relative_to(ROOT))+'\n' for f in files))
archive=REPO/'output/HW17-motor-electronics-r0.zip'
with zipfile.ZipFile(archive,'w',zipfile.ZIP_DEFLATED) as z:
 for f in files+[ROOT/'SHA256SUMS.txt']:z.write(f,Path('HW17-motor-electronics-r0')/f.relative_to(ROOT))
print(archive,archive.stat().st_size,'bytes')
