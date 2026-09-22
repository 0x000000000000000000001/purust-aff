module Test.Lifetime where

import Prelude
import Control.Monad.Error.Class (throwError)
import Data.Time.Duration (Milliseconds(..))
import Effect (Effect)
import Effect.Aff (launchAff_, delay, forkAff, error, never)
import Effect.Class (liftEffect)
import Effect.Console (log)
import Effect.Ref (Ref)
import Effect.Ref as Ref

foreign import scenario :: Int
foreign import panic :: Effect Unit

-- Starts are free: `launchAff_` returns before the parent fiber has run. Waiting
-- for the parent's return keeps the scenario order deterministic and makes sure
-- the `never` child exists before a scenario-3 panic. The wait is a short spin:
-- the parent signals from its first worker turn.
waitForReturn :: Ref Boolean -> Effect Unit
waitForReturn returned = do
  done <- Ref.read returned
  unless done (waitForReturn returned)

main :: Effect Unit
main = do
  returned <- Ref.new false
  launchAff_ do
    void $ forkAff do
      when (scenario == 3) never
      delay (Milliseconds 20.0)
      if scenario == 1 then throwError (error "intentional detached child failure")
      else if scenario == 2 then liftEffect panic
      else do
        void $ forkAff do
          delay (Milliseconds 20.0)
          liftEffect $ log "[OK] Aff grandchild completed after its parent"
        liftEffect $ log "[OK] Aff child completed after its parent"
    when (scenario == 1 || scenario == 2) $ void $ forkAff do
      if scenario == 2 then never else delay (Milliseconds 40.0)
      liftEffect $ log "[OK] remaining child completed after sibling failure"
    liftEffect $ log "[OK] Aff parent returned"
    liftEffect $ Ref.write true returned
  waitForReturn returned
  log "[OK] Effect main returned"
  when (scenario == 3) panic
