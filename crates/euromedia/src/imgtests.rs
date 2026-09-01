#[cfg(test)]
mod imgtests {
    use super::*;
    const PNG_RGBA: &[u8] = &[137,80,78,71,13,10,26,10,0,0,0,13,73,72,68,82,0,0,0,4,0,0,0,3,8,6,0,0,0,180,244,174,198,0,0,0,46,73,68,65,84,120,156,5,193,177,1,64,0,16,0,177,60,58,173,222,58,198,98,37,51,217,227,36,34,137,42,109,83,26,158,251,49,195,18,246,227,116,125,175,89,86,63,20,253,16,162,9,175,81,123,0,0,0,0,73,69,78,68,174,66,96,130];
    const PNG_RGB: &[u8] = &[137,80,78,71,13,10,26,10,0,0,0,13,73,72,68,82,0,0,0,4,0,0,0,3,8,2,0,0,0,59,150,57,145,0,0,0,41,73,68,65,84,120,156,99,248,207,192,192,240,159,129,129,225,255,127,24,249,191,161,161,129,129,129,129,233,63,3,3,183,136,188,199,147,77,140,76,204,0,41,30,13,155,133,162,199,223,0,0,0,0,73,69,78,68,174,66,96,130];
    const PNG_GREY: &[u8] = &[137,80,78,71,13,10,26,10,0,0,0,13,73,72,68,82,0,0,0,4,0,0,0,3,8,0,0,0,0,145,159,241,26,0,0,0,23,73,68,65,84,120,156,99,240,153,38,251,136,41,253,114,178,28,195,127,161,26,38,0,41,199,5,46,203,117,236,123,0,0,0,0,73,69,78,68,174,66,96,130];
    const BMP24: &[u8] = &[66,77,90,0,0,0,0,0,0,0,54,0,0,0,40,0,0,0,4,0,0,0,3,0,0,0,1,0,24,0,0,0,0,0,36,0,0,0,196,14,0,0,196,14,0,0,0,0,0,0,0,0,0,0,255,255,255,30,20,10,50,100,200,3,2,1,255,255,0,255,0,255,128,128,128,0,0,0,0,0,255,0,255,0,255,0,0,0,255,255];
    fn corners(img:&Image)->(Rgba,Rgba){ (img.get(0,0).unwrap(), img.get(3,2).unwrap()) }
    #[test] fn png_rgba(){ let i=decode_png(PNG_RGBA).unwrap(); assert_eq!(i.width,4); assert_eq!(i.height,3);
        assert_eq!(i.get(0,0).unwrap(),[255,0,0,255]); assert_eq!(i.get(3,2).unwrap(),[1,2,3,255]);
        assert_eq!(i.get(1,1).unwrap(),[255,0,255,255]); }
    #[test] fn png_rgb(){ let i=decode_png(PNG_RGB).unwrap(); assert_eq!(i.get(0,0).unwrap(),[255,0,0,255]); assert_eq!(i.get(2,0).unwrap(),[0,0,255,255]); }
    #[test] fn png_grey(){ let i=decode_png(PNG_GREY).unwrap(); assert_eq!(i.width,4);
        let g=i.get(0,0).unwrap(); assert_eq!(g[0],g[1]); assert_eq!(g[1],g[2]); assert_eq!(g[3],255); }
    #[test] fn bmp24(){ let i=decode_bmp(BMP24).unwrap(); assert_eq!(i.width,4); assert_eq!(i.height,3);
        assert_eq!(i.get(0,0).unwrap(),[255,0,0,255]); assert_eq!(i.get(3,2).unwrap(),[1,2,3,255]); }
    #[test] fn png_roundtrip(){ let i=decode_png(PNG_RGBA).unwrap(); let enc=encode_png(&i); let d=decode_png(&enc).unwrap();
        assert_eq!(i.pixels,d.pixels); }
    #[test] fn paint_canvas_roundtrip(){
        let mut im=Image::new(40,30,[255,255,255,255]);
        for x in 5..35 { im.set(x,15,[231,76,60,255]); }
        let png=encode_png(&im); let d=decode_png(&png).unwrap();
        assert_eq!(d.width,40); assert_eq!(d.get(20,15).unwrap(),[231,76,60,255]);
        assert_eq!(d.get(0,0).unwrap(),[255,255,255,255]);
    }
}
