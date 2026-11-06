export type Endpoint={product:'alt'|'sentori';installation:string;workspace:string;scope:string;generation:string};
export type Release={application:string;release:string;release_name:string;environment:string;artifact_sha256:string};
export type Delivery={source:{installation:string;repository:string;epoch:string};provenance_digest:string;publication_digest:string;receipt_digest:string;task:string;integration:string;delivery:string;native_commit:string;git_commit:string};
export type LinkBody=
 |{kind:'mapping';value:{delivery:Delivery;release:Release;declared_by:string}}
 |{kind:'withdraw';value:{previous_mapping_digest:string;reason:string;declared_by:string}}
 |{kind:'repair_request';value:{issue:string;selected_samples:string[];target_ref:string;requested_by:string;mapping_event_digest:string|null;observed_version:{application:string|null;release:string|null;release_name:string|null;environment:string|null}}}
 |{kind:'task_bound';value:{repair_request_digest:string;task:string;publication_digest:string}}
 |{kind:'observation';value:{mapping_event_digest:string;release:Release;start_unix_ms:string;end_unix_ms:string;collected_unix_ms:string;persisted_events:string;persisted_error_events:string;persisted_sessions:string;sampling:'persisted_only_unknown_coverage'}};
export type Decision={state:'applied'}|{state:'refused';reason:string};
export type LinkHistory={statement:{event:{version:1;channel:string;issuer:Endpoint;receiver:Endpoint;entity:string;revision:string;previous_digest:string|null;issued_unix_ms:string;body:LinkBody};signature:string};acknowledgement:{receipt:{body:{event_digest:string;decision:Decision}}}|null};
export type LinkSummary={digest:string;channel:string;entity:string;kind:LinkBody['kind'];revision:string;issued_unix_ms:string;decision:Decision|null;pending:boolean;can_reauthorize:boolean};
export type LinkPage={entries:LinkSummary[];next:string|null};
export type Channel={channel:string;application:string;peer:Endpoint;configuration_digest:string;enabled:boolean;peer_browser_origin?:string};
export type MappingRequest={key:import('./api').RequestKey;channel:string;configuration_digest:string;delivery_id:string;release:Release;expected_previous:string|null};
export type WithdrawalRequest={key:import('./api').RequestKey;channel:string;configuration_digest:string;mapping_event_digest:string;expected_previous:string;reason:string};
