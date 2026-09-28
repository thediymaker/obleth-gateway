import { redirect } from "next/navigation";

// Recipes live under Deployments now, as the first step of a launch.
export default function RecipesPage() {
  redirect("/deployments?tab=recipes");
}
