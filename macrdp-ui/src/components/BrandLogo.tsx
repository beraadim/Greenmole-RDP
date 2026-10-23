import logo from "../assets/greenmole.png";

/** The same Greenmole mascot used by the web client. */
export default function BrandLogo({ className = "", alt = "" }: {
  className?: string;
  alt?: string;
}) {
  return <img src={logo} alt={alt} className={`shrink-0 object-contain ${className}`} />;
}
