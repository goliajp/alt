-- Published Git objects live in the pack segments alone, and refs record no
-- retention in the content store. The migration step that goes with this version
-- moves what older versions kept there into segments, releases those copies with
-- their pins and recounts each repository's stored bytes; it validates this
-- constraint once no pin of a ref is left.
ALTER TABLE alt_pins ADD CONSTRAINT alt_pins_no_ref CHECK (pin_id NOT LIKE 'ref:%') NOT VALID;
