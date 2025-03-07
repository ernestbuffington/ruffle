// The initial version of this file was autogenerated from the official AS3 reference at
// https://help.adobe.com/en_US/FlashPlatform/reference/actionscript/3/flash/display3D/Context3DProfile.html
// by https://github.com/golfinq/ActionScript_Event_Builder
// It won't be regenerated in the future, so feel free to edit and/or fix

package flash.display3D
{

    [API("682")]
    public final class Context3DProfile
    {
        // Use the default feature support profile.
        public static const BASELINE:String = "baseline";

        // Use a constrained feature support profile to target older GPUs This profile is primarily targeted at devices that only support PS_2.0 level shaders like the Intel GMA 9xx series.
        public static const BASELINE_CONSTRAINED:String = "baselineConstrained";

        // Use an extended feature support profile to target newer GPUs which support larger textures This profile increases the maximum 2D Texture and RectangleTexture size to 4096x4096
        [API("690")]
        public static const BASELINE_EXTENDED:String = "baselineExtended";

        // Use an standard profile to target GPUs which support MRT, AGAL2 and float textures.
        [API("698")]
        public static const STANDARD:String = "standard";

        // Use an standard profile to target GPUs which support AGAL2 and float textures.
        [API("702")]
        public static const STANDARD_CONSTRAINED:String = "standardConstrained";

        // Use standard extended profile to target GPUs which support AGAL3 and instanced drawing feature.
        [API("704")] // the docs say 706, that's wrong
        public static const STANDARD_EXTENDED:String = "standardExtended";

    }
}
