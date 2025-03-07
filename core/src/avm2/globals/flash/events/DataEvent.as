// The initial version of this file was autogenerated from the official AS3 reference at 
// https://help.adobe.com/en_US/FlashPlatform/reference/actionscript/3/flash/events/DataEvent.html
// by https://github.com/golfinq/ActionScript_Event_Builder
// It won't be regenerated in the future, so feel free to edit and/or fix
package flash.events {
    public class DataEvent extends TextEvent {
        public static const DATA:String = "data"; // Defines the value of the type property of a data event object.
        public static const UPLOAD_COMPLETE_DATA:String = "uploadCompleteData"; // Defines the value of the type property of an uploadCompleteData event object.

        public function DataEvent(type:String, bubbles:Boolean = false, cancelable:Boolean = false, data:String = "") {
            super(type, bubbles, cancelable, data);
        }


        // `DataEvent.data` seems to delegate to the superclass's (TextEvent's) `TextEvent.text`.
        public function get data():String {
            return super.text;
        }

        public function set data(value:String):void {
            super.text = value;
        }


        //  Creates a copy of the DataEvent object and sets the value of each property to match that of the original.
        override public function clone():Event {
            return new DataEvent(this.type, this.bubbles, this.cancelable, this.data);
        }

        //  Returns a string that contains all the properties of the DataEvent object.
        override public function toString():String {
            return this.formatToString("DataEvent","type","bubbles","cancelable","eventPhase","data");
        }
    }
}
