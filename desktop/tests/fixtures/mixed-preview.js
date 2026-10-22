// Synthetic states for browser previews. No runtime, network peers or hardware.
function scenario(name = 'normal') {
  const builtin = name.includes('builtin') || name === 'closed-lid';
  const single = name === 'single' || name === 'closed-lid';
  const three = name === 'three' || name === 'unassigned';
  const count = builtin || single ? 1 : three ? 3 : 2;
  const devices = [{device_id:'mac',name:'MacBook Pro',port_index:1},{device_id:'pc',name:'游戏 PC',port_index:2}];
  if (three) devices.push({device_id:'mini',name:'Office-Mini',port_index:3});
  const monitors = Array.from({length:count},(_,i)=>({fingerprint:`screen-${i+1}`,label:'',local_input:i===1?7:15,source:'learned_active_read'}));
  for(const device of devices) device.monitors=monitors.map((m,i)=>({...m,local_input:device.device_id==='mac'?m.local_input:i===1?8:16}));
  const enabled = ['mixed','builtin-offline','unassigned','three'].includes(name);
  const assignment=monitors.filter((m,i)=>name!=='unassigned'||i<2).map((m,i)=>({fingerprint:m.fingerprint,device_id:name==='all-to-one'?'pc':i===1?'mac':'pc'}));
  const validation={ok:!single && name!=='all-to-one',reasons:[]};
  if(single) validation.reasons.push({code:'not_enough_screens',severity:'error'});
  if(name==='all-to-one') validation.reasons.push({code:'all_to_one_computer',severity:'error'});
  if(three) validation.reasons.push({code:'computer_without_screen',severity:'warning',device_id:'mini'});
  if(name==='unassigned') validation.reasons.push({code:'unassigned_monitor',severity:'warning',fingerprint:'screen-3'});
  const mixed={enabled,revision:3,section_meta:{updated_at_ms:123,updated_by:'mac'},assignment,validation,keyboard_holder_device_id:'mac',holder_confidence:'known',
    destinations:monitors.map((m,i)=>({fingerprint:m.fingerprint,builtin:false,available:true,connected:!(enabled&&assignment[i]?.device_id==='pc'),
      fixed:enabled&&!!assignment[i],current_device_id:enabled?null:'mac',target_device_id:enabled?assignment[i]?.device_id||'mac':'mac',evidence:enabled?'commanded_unconfirmed':'input_observation_only'})),
    transition:{phase:name==='restoring'?'restoring':'idle',started_ms:123,outcome:name==='restore-pending'?'partial':'ok',reasons:[]},
    pending_remote:name==='restore-pending'?[{id:'mixed:'+JSON.stringify(['preview',3,123,'mac','screen-1','mac']),device_id:'pc',fingerprint:'screen-1',target_device_id:'mac'}]:[],warnings:[],execution:[],topology_error:null,simulated:true};
  if(builtin) mixed.destinations.push({fingerprint:'builtin',builtin:true,available:name!=='closed-lid',connected:name!=='closed-lid',unavailable_reason:name==='closed-lid'?'builtin_closed_or_unavailable':null,current_device_id:'mac',target_device_id:'mac',evidence:'local_topology'});
  const config={schema_version:2,local_device:{device_id:'mac',host_label:'MacBook Pro',monitors},switch_group:{group_id:'preview',revision:1,devices,mixed:{enabled,revision:3,assignment}},
    trigger:{anchor:{vid_pid:'1234:5678'},members:[{vid_pid:'2345:6789'}],debounce:{t_stable_ms:0,t_absent_ms:0,t_cooldown_ms:1000,quorum_peripherals:1}},
    advanced:{ddc_retry:{attempts:2,delay_ms:100},usb_poll_ms:250,arrival_correction_enabled:false}};
  const state={enabled:true,state:'idle',mixed};
  if(name==='builtin-offline') state.lan={peers:[{device_id:'pc',name:'游戏 PC',online:false,synced:false}]};
  return {config,state};
}
if(typeof module!=='undefined') module.exports={scenario};
