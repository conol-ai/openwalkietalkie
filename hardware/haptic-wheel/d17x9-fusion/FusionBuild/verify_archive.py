"""Open the delivered native archive and verify its editable content survives."""
original_document=app.activeDocument
im=app.importManager
step_doc=im.importToNewDocument(im.createSTEPImportOptions(str(HERE/'HW17-R0-Fusion.step')))
step_design=adsk.fusion.Design.cast(app.activeProduct)
step_bodies=list(step_design.rootComponent.bRepBodies)
for so in step_design.rootComponent.allOccurrences:
    step_bodies.extend(list(so.bRepBodies))
step_report={'reopened_in_fusion':True,'solid_count':len(step_bodies),'all_solid':all(b.isSolid for b in step_bodies),'volume_mm3':sum(b.physicalProperties.volume*1000 for b in step_bodies)}
expected_volume=sum(v['volume_mm3'] for v in META.values())
step_report['volume_error_mm3']=step_report['volume_mm3']-expected_volume
assert len(step_bodies)==41 and step_report['all_solid'] and abs(step_report['volume_error_mm3'])<1e-5
(HERE/'step-check.json').write_text(json.dumps(step_report,indent=2),encoding='utf-8')
step_doc.close(False)
opened=im.importToNewDocument(im.createFusionArchiveImportOptions(str(HERE/'HW17-R0-Fusion.f3d')))
assert opened
check=adsk.fusion.Design.cast(app.activeProduct)
rr=check.rootComponent
parts=[]
for occurrence in rr.allOccurrences:
    for body in occurrence.component.bRepBodies:
        bb=body.preciseBoundingBox
        parts.append({'name':body.name,'is_solid':body.isSolid,'volume_mm3':body.physicalProperties.volume*1000})
warnings=[]
for item in check.timeline:
    ent=item.entity
    if hasattr(ent,'healthState') and ent.healthState!=adsk.fusion.FeatureHealthStates.HealthyFeatureHealthState:
        warnings.append(getattr(ent,'errorOrWarningMessage',''))
report={'native_reopened':True,'part_count':len(parts),'timeline_count':check.timeline.count,'parametric':check.designType==adsk.fusion.DesignTypes.ParametricDesignType,'as_built_joints':rr.asBuiltJoints.count,'section_analyses':check.analyses.sectionAnalyses.count,'named_views':check.namedViews.count,'warnings':warnings,'parts':parts}
assert len(parts)==41 and all(p['is_solid'] for p in parts)
assert report['timeline_count']==235 and report['as_built_joints']==1
assert report['section_analyses']==2 and not warnings
entities=adsk.core.ObjectCollection.create()
for o in rr.allOccurrences:
    for b in o.bRepBodies: entities.add(b)
interference=check.analyzeInterference(check.createInterferenceInput(entities))
report['interferences']=[{'entity_one':r.entityOne.name,'entity_two':r.entityTwo.name,'volume_mm3':r.interferenceBody.physicalProperties.volume*1000} for r in interference]
assert not report['interferences'],report['interferences']
(HERE/'archive-roundtrip.json').write_text(json.dumps(report,indent=2,ensure_ascii=False),encoding='utf-8')
original_document.close(False)
opened.activate()
check.namedViews.itemByName('01 总装轴测 | Assembly ISO').apply()
app.activeViewport.fit()
log('NATIVE ARCHIVE REOPEN VERIFIED')
